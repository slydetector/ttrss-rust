#!/usr/bin/env python3
"""Seed the oracle with the golden dataset.

Run against a FRESH stack (scripts/up.sh, no prior seeding). The script:
  1. creates the category tree (nested)
  2. creates labels
  3. subscribes feeds (live fetch) via the external API, inserts the
     deliberately-broken feeds directly
  4. runs feed updates until stable
  5. adds realistic per-article state (read/published/notes/scores/tags/
     labels), filters, profiles, app password, TOTP, access keys, prefs,
     and a second user
  6. writes oracle/data/ids.json (resolved ids for the scenario template)

The updater daemon should be stopped before recording corpus runs (the DB is
then frozen); seeding itself is fine with the daemon running.
"""

import hashlib
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from http.cookiejar import CookieJar
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ORACLE = ROOT / "oracle"
DATA = ORACLE / "data"


def env_get(key: str, default: str | None = None) -> str | None:
    for line in (ORACLE / ".env").read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        if k == key:
            return v
    return default


BASE = f"http://127.0.0.1:{env_get('HTTP_PORT', '8280')}/tt-rss"
ADMIN_LOGIN = "admin"
ADMIN_PASS = env_get("ADMIN_USER_PASS", "admin")
RO_LOGIN = env_get("AUTO_CREATE_USER", "readonly")
RO_PASS = env_get("AUTO_CREATE_USER_PASS", "readonlypass")

CATS = [
    ("News", None),
    ("World News", "News"),
    ("Tech News", "News"),
    ("Development", None),
    ("Rust", "Development"),
    ("Blogs", "Development"),
    ("Science", None),
    ("Media", None),
    ("Broken", None),
]

LABELS = [
    "Important",
    "Read later",
    "Project X",
    "Inbox",
    "Archive",
    "Someday",
    "Work",
    "Personal",
]

BROKEN_FEEDS = {
    "https://nonexistent-tt-rust-oracle-8280.example.com/feed.xml": "Dead Feed (DNS failure)",
    "https://arstechnica.com/no-such-feed-path-404": "404 Feed",
    "https://example.com/": "HTML Page (not a feed)",
}


def sh(cmd: list[str], cwd=ORACLE, check: bool = True) -> str:
    p = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    if check and p.returncode != 0:
        raise RuntimeError(f"command failed: {cmd}\n{p.stderr}")
    return p.stdout


def sql(query: str) -> list[list[str]]:
    """Run a SQL query in the db container; return rows as lists of strings."""
    p = subprocess.run(
        ["docker", "compose", "exec", "-T", "db", "psql", "-U", "postgres", "-d", "postgres",
         "-t", "-A", "-F", "\t", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, cwd=ORACLE)
    if p.returncode != 0:
        raise RuntimeError(f"SQL failed:\n{query}\n{p.stderr}")
    return [line.split("\t") for line in p.stdout.splitlines() if line.strip()]


def sql_exec(query: str) -> None:
    sql(query)


def q(s: str) -> str:
    """Single-quote a SQL string literal."""
    return "'" + s.replace("'", "''") + "'"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Client:
    def __init__(self):
        self.jar = CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar), NoRedirect())

    def request(self, method: str, path: str, form=None, json_body=None,
                headers=None, expect=None) -> tuple[int, bytes]:
        data = None
        hdrs = dict(headers or {})
        if form is not None:
            data = urllib.parse.urlencode(form, doseq=True).encode()
            hdrs.setdefault("Content-Type", "application/x-www-form-urlencoded")
        if json_body is not None:
            data = json.dumps(json_body).encode()
            hdrs["Content-Type"] = "application/json"
        req = urllib.request.Request(BASE + path, data=data, headers=hdrs, method=method)
        try:
            resp = self.opener.open(req, timeout=180)
            status, body, rheaders = resp.status, resp.read(), dict(resp.headers)
        except urllib.error.HTTPError as e:
            status, body, rheaders = e.code, e.read(), dict(e.headers)
        if expect is not None and status != expect:
            raise RuntimeError(
                f"{method} {path}: expected {expect}, got {status}\n{body[:800]!r}")
        return status, body


def web_login(c: Client, user: str, password: str) -> str:
    c.request("GET", "/index.php", expect=200)  # establish session cookie
    c.request("POST", f"/public.php?return={BASE}/index.php",
              form={"op": "login", "login": user, "password": password},
              expect=302)
    # The post-login shell carries this session's CSRF token.
    _, body = c.request("GET", "/index.php", expect=200)
    m = re.search(rb'const __csrf_token = "([0-9a-f]+)"', body)
    if not m:
        raise RuntimeError("csrf token not found in post-login shell")
    return m.group(1).decode()


def backend(c: Client, csrf: str, op: str, method: str, **params) -> bytes:
    form = {"op": op, "method": method, "csrf_token": csrf}
    form.update(params)
    _, body = c.request("POST", "/backend.php", form=form, expect=200)
    return body


def api(c: Client, payload: dict, sid: str | None = None) -> dict:
    if sid:
        payload = {"sid": sid, **payload}
    _, body = c.request("POST", "/api/index.php", json_body=payload, expect=200)
    j = json.loads(body)
    if j.get("status") != 0:
        raise RuntimeError(f"API {payload.get('op')!r} failed: {j}")
    return j.get("content") or {}


def main() -> int:
    DATA.mkdir(parents=True, exist_ok=True)

    # The updater daemon would race our CLI update passes; stop it for the
    # whole seeding process (it stays stopped for corpus recording too).
    sh(["docker", "compose", "stop", "updater"])

    # 0. wait for health
    c_health = Client()
    for _ in range(60):
        try:
            s, b = c_health.request("GET", "/healthz")
            if s == 200 and b.strip() == b"OK":
                break
        except urllib.error.HTTPError:
            pass
        time.sleep(2)
    else:
        print("oracle not healthy", file=sys.stderr)
        return 1
    print(f"oracle healthy at {BASE}")

    c = Client()
    csrf = web_login(c, ADMIN_LOGIN, ADMIN_PASS)
    print("logged in as admin (web)")

    # 1. category tree
    admin_uid = int(sql("select id from ttrss_users where login = 'admin'")[0][0])
    cat_ids: dict[str, int] = {}
    for title, parent in CATS:
        parent_expr = "null" if parent is None else str(cat_ids["/" + parent])
        sql_exec(
            f"insert into ttrss_feed_categories (owner_uid, title, parent_cat) "
            f"values ({admin_uid}, {q(title)}, {parent_expr})")
        cat_ids["/" + title] = int(
            sql(f"select id from ttrss_feed_categories where owner_uid = {admin_uid} and title = {q(title)}")[0][0])
    # bare + nested path keys, as used by feeds.txt
    for title, parent in CATS:
        cat_ids[title] = cat_ids["/" + title]
        if parent:
            cat_ids[f"{parent}/{title}"] = cat_ids["/" + title]
    print("categories:", cat_ids)

    # collapse one category for UI coverage
    sql_exec(f"update ttrss_feed_categories set collapsed = true "
             f"where id = {cat_ids['/Blogs']}")

    # 2. labels (via the real backend handler)
    for cap in LABELS:
        backend(c, csrf, "Pref_Labels", "add", caption=cap, output="1")
    label_ids = {}
    for row in sql(f"select caption, id from ttrss_labels2 where owner_uid = {admin_uid} order by id"):
        label_ids[row[0]] = int(row[1])
    print("labels:", label_ids)

    # 3. per-user prefs (admin) — ENABLE_API_ACCESS must be set before the
    #    API login below (it defaults to false).
    admin_prefs = {
        "_ENABLED_PLUGINS": "note,nsfw,share,bookmarklets,auto_assign_labels",
        "USER_CSS_THEME": "night",
        "USER_LANGUAGE": "en",
        "COMBINED_DISPLAY_MODE": "true",
        "WIDESCREEN_MODE": "false",
        "DIGEST_ENABLE": "true",
        "DIGEST_PREFERRED_TIME": "08:00",
        "SHORT_DATE_FORMAT": "j M, G:i",
        "_DEFAULT_VIEW_MODE": "list",
        "ENABLE_API_ACCESS": "true",
        "_COLLAPSED_UNCAT": "true",
    }
    set_pref = (
        "insert into ttrss_user_prefs2 (owner_uid, pref_name, value) "
        "select {uid}, {name}, {val} where not exists "
        "(select 1 from ttrss_user_prefs2 where owner_uid = {uid} "
        "and pref_name = {name} and profile is null)")
    upd_pref = (
        "update ttrss_user_prefs2 set value = {val} where owner_uid = {uid} "
        "and pref_name = {name} and profile is null")
    for name, value in admin_prefs.items():
        sql_exec(set_pref.format(uid=admin_uid, name=q(name), val=q(value)))
        sql_exec(upd_pref.format(uid=admin_uid, name=q(name), val=q(value)))
    print("admin prefs set")

    # 4. feeds: parse feeds.txt
    feeds: list[tuple[str, str]] = []
    for line in (ORACLE / "feeds.txt").read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        cat_path, url = [x.strip() for x in line.split("|", 1)]
        feeds.append((cat_path, url))

    # api login
    content = api(c, {"op": "login", "user": ADMIN_LOGIN, "password": ADMIN_PASS})
    api_sid = content["session_id"]
    print("api session:", api_sid[:12], "...")

    feed_ids: dict[str, int] = {}
    for cat_path, url in feeds:
        if cat_path == "Broken":
            continue  # inserted directly below
        cat_id = cat_ids.get(cat_path, 0)
        content = api(c, {"op": "subscribetofeed", "feed_url": url,
                          "category_id": cat_id}, sid=api_sid)
        # API wraps _subscribe()'s array under the "status" key
        st = content.get("status") if isinstance(content, dict) else None
        if isinstance(st, dict) and st.get("code") in (0, 1):
            fid = int(st.get("feed_id", 0))
        else:
            print(f"  warning: subscribe {url} -> {content}", file=sys.stderr)
            fid = 0
        feed_ids[url] = fid

    # broken feeds (direct SQL so they exist despite failing fetches)
    for url, title in BROKEN_FEEDS.items():
        sql_exec(
            "insert into ttrss_feeds (owner_uid, title, feed_url, cat_id, "
            "update_method, update_interval, auth_login, auth_pass, "
            "auth_pass_encrypted) "
            f"values ({admin_uid}, {q(title)}, {q(url)}, "
            f"{cat_ids.get('Broken', 0)}, 0, 0, '', '', false)")
        feed_ids[url] = int(
            sql(f"select id from ttrss_feeds where owner_uid = {admin_uid} and feed_url = {q(url)}")[0][0])
    print(f"feeds: {len(feed_ids)} total, "
          f"{sum(1 for v in feed_ids.values() if v)} subscribed/inserted")

    # 5. run feed updates until stable (2 passes: first imports, second settles)
    #    (sudo -E: the CLI needs the TTRSS_* env vars to reach the DB)
    for i in range(2):
        print(f"running feed update pass {i + 1} ...")
        out = sh(["docker", "compose", "exec", "-T", "app", "sh", "-c",
                  "sudo -E -u app php /var/www/html/tt-rss/update.php --feeds 2>&1"])
        last = "\n".join(out.splitlines()[-6:])
        print(last)
    n_articles = int(sql("select count(*) from ttrss_entries")[0][0])
    n_updated = int(sql("select count(*) from ttrss_feeds where last_updated is not null")[0][0])
    n_errors = int(sql("select count(*) from ttrss_feeds where last_error <> ''")[0][0])
    print(f"entries: {n_articles}, feeds updated: {n_updated}, feeds with errors: {n_errors}")
    if n_articles < 100:
        print("warning: fewer than 100 articles; check feed fetching", file=sys.stderr)
    pending = int(sql("select count(*) from ttrss_feeds where last_updated is null")[0][0])
    if pending:
        print(f"warning: {pending} feeds still pending update", file=sys.stderr)

    # 6. per-article state (deterministic by id)
    tags = ["news", "deep-dive", "fun", "important", "linux", "security"]
    note_texts = [
        "Seeded note A: worth revisiting.",
        "Seeded note B: see thread in comments.",
        "Seeded note C: links to related project.",
        "Seeded note D: counter-argument noted.",
    ]
    label_list = list(label_ids.items())  # (caption, id)
    sql_exec(
        f"update ttrss_user_entries set marked = true, unread = false, "
        f"last_read = now(), last_marked = now() "
        f"where owner_uid = {admin_uid} and int_id % 7 = 0")
    sql_exec(
        f"update ttrss_user_entries set published = true, last_published = now() "
        f"where owner_uid = {admin_uid} and int_id % 97 = 0")
    for i, note in enumerate(note_texts):
        sql_exec(
            f"update ttrss_user_entries set note = {q(note)} "
            f"where owner_uid = {admin_uid} and int_id % 29 = {i}")
    sql_exec(
        f"update ttrss_user_entries set score = (1 + int_id % 5) "
        f"where owner_uid = {admin_uid} and int_id % 13 = 0")
    for i, tag in enumerate(tags):
        ids = [r[0] for r in sql(
            f"select int_id from ttrss_user_entries where owner_uid = {admin_uid} and int_id % 31 = {i}")]
        if ids:
            sql_exec(
                f"insert into ttrss_tags (tag_name, owner_uid, post_int_id) "
                f"select {q(tag)}, {admin_uid}, unnest(ARRAY{ids}::int[]) "
                f"on conflict do nothing")
            sql_exec(
                f"update ttrss_user_entries set tag_cache = {q(tag)} "
                f"where owner_uid = {admin_uid} and int_id in ({','.join(ids)})")
    for i, (cap, lid) in enumerate(label_list):
        ids = [r[0] for r in sql(
            f"select int_id from ttrss_user_entries where owner_uid = {admin_uid} and int_id % 17 = {i}")]
        if ids:
            sql_exec(
                f"insert into ttrss_user_labels2 (label_id, article_id) "
                f"select {lid}, unnest(ARRAY{ids}::int[]) on conflict do nothing")
            sql_exec(
                f"update ttrss_user_entries set label_cache = {q(cap)} "
                f"where owner_uid = {admin_uid} and int_id in ({','.join(ids)})")
    print("article state seeded")

    # 7. filters (2 realistic ones)
    tech_cat_id = cat_ids.get("News/Tech News", 0)
    hn_url = "https://hnrss.org/frontpage"
    hn_feed_id = feed_ids.get(hn_url, 0)
    sql_exec(
        "insert into ttrss_filters2 (owner_uid, title, enabled, order_id, match_any_rule, inverse) "
        f"values ({admin_uid}, 'Auto: security', true, 1, false, false) returning id")
    f1 = int(sql(f"select id from ttrss_filters2 where owner_uid = {admin_uid} and title = 'Auto: security'")[0][0])
    sql_exec(
        "insert into ttrss_filters2_rules (filter_id, reg_exp, filter_type, cat_id) "
        f"values ({f1}, '(?i)security', 1, {tech_cat_id})")
    sql_exec(
        "insert into ttrss_filters2_actions (filter_id, action_id, action_param) "
        f"values ({f1}, 3, '')")  # mark
    sql_exec(
        "insert into ttrss_filters2_actions (filter_id, action_id, action_param) "
        f"values ({f1}, 7, {str(label_ids.get('Important', 0))})")  # label

    sql_exec(
        "insert into ttrss_filters2 (owner_uid, title, enabled, order_id, match_any_rule, inverse) "
        f"values ({admin_uid}, 'Score Ask HN', true, 2, false, false) returning id")
    f2 = int(sql(f"select id from ttrss_filters2 where owner_uid = {admin_uid} and title = 'Score Ask HN'")[0][0])
    sql_exec(
        "insert into ttrss_filters2_rules (filter_id, reg_exp, filter_type, feed_id) "
        f"values ({f2}, '(?i)^Ask HN', 1, {hn_feed_id})")
    sql_exec(
        "insert into ttrss_filters2_actions (filter_id, action_id, action_param) "
        f"values ({f2}, 6, '3')")  # score 3
    print("filters seeded")

    # 8. profiles (admin)
    for title in ("Home", "Work"):
        sql_exec(
            "insert into ttrss_settings_profiles (title, owner_uid) "
            f"values ({q(title)}, {admin_uid})")
    profile_ids = {}
    for row in sql(f"select title, id from ttrss_settings_profiles where owner_uid = {admin_uid} order by id"):
        profile_ids[row[0]] = int(row[1])
    sql_exec(
        "insert into ttrss_user_prefs2 (owner_uid, pref_name, profile, value) "
        f"values ({admin_uid}, 'USER_CSS_THEME', {profile_ids['Home']}, 'light'),"
        f"       ({admin_uid}, 'USER_CSS_THEME', {profile_ids['Work']}, 'night_blue')")
    print("profiles:", profile_ids)

    # 9. app password + TOTP for admin
    # App password for the API login service. Note: service must be the auth
    # service name ('_api'), check_app_password() matches on it exactly.
    # App password hash format is ALGO:HASH:SALT (salt AFTER the hash), unlike
    # user passwords.
    ap_pass = "AppPass123"
    ap_salt = "a1b2c3d4e5f6"
    ap_hash = f"SSHA-512:{hashlib.sha512((ap_salt + ap_pass).encode()).hexdigest()}:{ap_salt}"
    sql_exec(
        "insert into ttrss_app_passwords (title, pwd_hash, service, created, owner_uid) "
        f"values ('RSS Reader App', {q(ap_hash)}, '_api', now(), {admin_uid})")
    sql_exec(
        "update ttrss_users set otp_enabled = true, otp_secret = 'JBSWY3DPEHPK3PXP' "
        f"where id = {admin_uid}")
    print("app password + TOTP set for admin")

    # 10. access keys (feed + category) for syndication (unchanged numbering)
    first_feed_id = feed_ids.get("https://arstechnica.com/information-technology/feed/", 0)
    ak_feed = "a1b2c3d4e5f6g"
    ak_cat = "g6d4c3b2a1e8f"
    sql_exec(
        "insert into ttrss_access_keys (access_key, feed_id, is_cat, owner_uid) "
        f"values ({q(ak_feed)}, {str(first_feed_id)}, false, {admin_uid})")
    sql_exec(
        "insert into ttrss_access_keys (access_key, feed_id, is_cat, owner_uid) "
        f"values ({q(ak_cat)}, {str(tech_cat_id)}, true, {admin_uid})")
    print("access keys:", ak_feed, ak_cat)

    # 11. second user via the CLI (uses the real password hashing code)
    # (numbering kept stable; see steps above)
    sh(["docker", "compose", "exec", "-T", "app", "sh", "-c",
        'sudo -E -u app php /var/www/html/tt-rss/update.php --force-yes '
        '--user-add "testuser:testpass:0" 2>&1'])
    test_uid = int(sql("select id from ttrss_users where login = 'testuser'")[0][0])
    sql_exec(
        "insert into ttrss_user_prefs2 (owner_uid, pref_name, value) values "
        f"({test_uid}, 'USER_CSS_THEME', 'compact_night'),"
        f"({test_uid}, '_ENABLED_PLUGINS', 'note'),"
        f"({test_uid}, 'USER_LANGUAGE', 'en')")
    print("testuser created, uid:", test_uid)

    # Fixed last_login for all users: the users index page displays it, and
    # logins performed during the corpus would otherwise leak state between
    # runs ("Never" on the first run, a time on subsequent ones).
    sql_exec(
        "update ttrss_users set last_login = '2026-01-01 12:00:00' "
        "where last_login is null")
    ro_uid = int(sql(f"select id from ttrss_users where login = {q(RO_LOGIN)}")[0][0])

    # 12. sample articles for the scenario template
    # (numbering kept stable)
    def first_article(feed_id: int) -> int:
        rows = sql(
            f"select int_id from ttrss_user_entries where owner_uid = {admin_uid} "
            f"and feed_id = {feed_id} order by ref_id desc limit 1")
        return int(rows[0][0]) if rows else 0

    ids = {
        "base_url": BASE,
        "admin": {"uid": admin_uid, "login": ADMIN_LOGIN, "password": ADMIN_PASS},
        "readonly": {"uid": ro_uid, "login": RO_LOGIN, "password": RO_PASS},
        "testuser": {"uid": test_uid, "login": "testuser", "password": "testpass"},
        "categories": {
            "news": cat_ids.get("News", 0),
            "world_news": cat_ids.get("News/World News", 0),
            "tech_news": cat_ids.get("News/Tech News", 0),
            "development": cat_ids.get("Development", 0),
            "rust": cat_ids.get("Development/Rust", 0),
            "blogs": cat_ids.get("Development/Blogs", 0),
            "science": cat_ids.get("Science", 0),
            "media": cat_ids.get("Media", 0),
            "broken": cat_ids.get("Broken", 0),
        },
        "labels": label_ids,
        "profiles": profile_ids,
        "feeds": {url: fid for url, fid in feed_ids.items() if fid},
        "feed_ars": feed_ids.get("https://arstechnica.com/information-technology/feed/", 0),
        "feed_hn_frontpage": feed_ids.get("https://hnrss.org/frontpage", 0),
        "article_1": first_article(feed_ids.get("https://arstechnica.com/information-technology/feed/", 0)),
        "article_2": first_article(feed_ids.get("https://hnrss.org/frontpage", 0)),
        "access_key_feed_ars": ak_feed,
        "access_key_cat_tech": ak_cat,
        "app_password": ap_pass,
        "totp_secret": "JBSWY3DPEHPK3PXP",
        "counts": {"entries": n_articles, "feeds": len(feed_ids),
                   "feeds_updated": n_updated, "feeds_errors": n_errors},
    }
    out = DATA / "ids.json"
    out.write_text(json.dumps(ids, indent=2) + "\n")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
