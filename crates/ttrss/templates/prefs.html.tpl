<!DOCTYPE html>
<html>
<head>
	<title>Tiny Tiny RSS : Preferences</title>
    <meta name="viewport" content="initial-scale=1,width=device-width" />

	{{ theme_css_link }}
	
	<script type="text/javascript">
		const __csrf_token = "{{ csrf_token }}";

		const __default_light_theme = "themes/light.css?{{ scripts_ts }}";
		const __default_dark_theme = "themes/night.css?{{ scripts_ts }}";
	</script>

	
	{{ user_css_style }}
	<style>
		{{ plugin_css }}
	</style>

	<link rel="shortcut icon" type="image/png" href="images/favicon.png"/>
	<link rel="icon" type="image/png" sizes="72x72" href="images/favicon-72px.png" />

	<script>
		dojoConfig = {
			async: true,
			cacheBust: "{{ scripts_ts }}",
			packages: [
				{ name: "lib", location: "../" },
				{ name: "fox", location: "../../js" },
			]
		};
	</script>

	<script src="lib/dojo/dojo.js?{{ scripts_ts }}" type="text/javascript" charset="utf-8"></script>
<script src="lib/dojo/tt-rss-layer.js?{{ scripts_ts }}" type="text/javascript" charset="utf-8"></script>
<script src="js/common.js?{{ scripts_ts }}" type="text/javascript" charset="utf-8"></script>
<script src="js/prefs.js?{{ scripts_ts }}" type="text/javascript" charset="utf-8"></script>

    <script type="text/javascript">
		require({cache:{}});
    </script>

{{ plugin_js }}

	<style type="text/css">
		@media (prefers-color-scheme: dark) {
			body {
				background : #303030;
			}
		}

		body.css_loading * {
			display : none;
		}
	</style>

	<noscript>
		<style type="text/css">
			body.css_loading noscript {
				display : block;
				margin : 16px;
			}
		</style>

		<link href="themes/light.css?{{ scripts_ts }}" rel="stylesheet" type="text/css" data-orig-href="themes/light.css"/>
	</noscript>

	<meta http-equiv="Content-Type" content="text/html; charset=utf-8"/>
</head>

<body class="flat ttrss_main ttrss_prefs css_loading">

<noscript class="alert alert-error">{{ __("Javascript is disabled. Please enable it.") }}</noscript>

<div id="notify" class="notify"></div>
<div id="cmdline" style="display : none"></div>

<div id="overlay">
	<div id="overlay_inner">
		{{ __("Loading, please wait...") }}		<div dojoType="dijit.ProgressBar" places="0" style="width : 300px" id="loading_bar"
	     progress="0" maximum="100">
		</div>
	</div>
</div>

<div id="header">
	<i class="material-icons net-alert" style="display: none"
		title="{{ __("Communication problem with server.") }}">error_outline</i>
	<i class="material-icons log-alert" style="display: none"
		 title="{{ __("Recent entries found in event log.") }}" onclick="App.openPreferences('system')">warning</i>
	<a id="updates-available" target="_blank" rel="noopener noreferrer" href="" style="display: none">
		<i class="material-icons icon-new-version" title="{{ __("Updates for Tiny Tiny RSS are available.") }}">new_releases</i>
	</a>
	<a id="plugin-updates-available" href="prefs.php" onclick="dijit.byId('pref-tabs').selectChild(dijit.byId('prefsTab')); return false" style="display: none">
		<i class="material-icons icon-new-version" title="{{ __("Updates for some local plugins are available.") }}">new_releases</i>
	</a>
	<a href="#" onclick="document.location.href = 'index.php'">{{ __("Exit preferences") }}</a>
</div>

<div id="main" dojoType="dijit.layout.BorderContainer">
    <div dojoType="dijit.layout.TabContainer" region="center" id="pref-tabs">
        <div id="prefsTab" dojoType="dijit.layout.ContentPane"
            href="backend.php?op=Pref_Prefs"
            title="<i class='material-icons'>settings</i> {{ __("Preferences") }}"></div>
        <div id="feedsTab" dojoType="dijit.layout.ContentPane"
            href="backend.php?op=Pref_Feeds"
            title="<i class='material-icons'>rss_feed</i>  {{ __("Feeds") }}"></div>
        <div id="filtersTab" dojoType="dijit.layout.ContentPane"
            style="padding : 0px"
            href="backend.php?op=Pref_Filters"
            title="<i class='material-icons'>filter_list1</i> {{ __("Filters") }}"></div>
        <div id="labelsTab" dojoType="dijit.layout.ContentPane"
            style="padding : 0px"
            href="backend.php?op=Pref_Labels"
            title="<i class='material-icons'>label_outline1</i> {{ __("Labels") }}"></div>
        {% if is_admin %}
        <div id="usersTab" dojoType="dijit.layout.ContentPane"
            style="padding : 0px"
            href="backend.php?op=Pref_Users"
            title="<i class='material-icons'>person</i> {{ __("Users") }}"></div>
        <div id="systemTab" dojoType="dijit.layout.ContentPane"
            href="backend.php?op=Pref_System"
            title="<i class='material-icons'>info_outline</i> {{ __("System") }}"></div>
        {% endif %}
        {{ prefs_tabs_hooks }}
                        </div>
		<div id="footer" dojoType="dijit.layout.ContentPane" region="bottom">
			<a class="text-muted" target="_blank" href="https://github.com/tt-rss/tt-rss">Tiny Tiny RSS</a>
				<span>v<span title="2026-08-30 18:17:30
2f7d831f
main">26.08-2f7d831f</span></span>
    </div>
</div>

</body>
</html>
