<!DOCTYPE html>
<html>
<head>
	<title>Tiny Tiny RSS</title>
    <meta name="viewport" content="initial-scale=1,width=device-width" />

	__THEME_CSS_LINK__
	
	<script type="text/javascript">
		const __csrf_token = "__CSRF_TOKEN__";

		const __default_light_theme = "themes/light.css?__SCRIPTS_TS__";
		const __default_dark_theme = "themes/night.css?__SCRIPTS_TS__";
	</script>

	
	<style type="text/css">
	.article-note[data-note-for] {
   cursor : pointer;
}
details.nsfw {
				cursor : pointer;
				user-select : none;
			}:is(.ttrss_main .post .header, .cdm .footer) i.material-icons.icon-share.is-shared {
    color : #0a0;
}
	</style>

	<link rel="shortcut icon" type="image/png" href="images/favicon.png"/>
	<link rel="icon" type="image/png" sizes="72x72" href="images/favicon-72px.png" />

	<!-- Progressive Web App: installable, launches standalone on mobile. -->
	<link rel="manifest" href="manifest.webmanifest"/>
	<meta name="theme-color" content="#257aa7" media="(prefers-color-scheme: light)"/>
	<meta name="theme-color" content="#303030" media="(prefers-color-scheme: dark)"/>
	<link rel="apple-touch-icon" href="images/icon-192.png"/>
	<meta name="apple-mobile-web-app-capable" content="yes"/>
	<meta name="mobile-web-app-capable" content="yes"/>
	<meta name="apple-mobile-web-app-status-bar-style" content="default"/>
	<meta name="apple-mobile-web-app-title" content="Tiny Tiny RSS"/>

	<script>
		dojoConfig = {
			async: true,
			cacheBust: "__SCRIPTS_TS__",
			packages: [
				{ name: "fox", location: "../../js" },
			]
		};
	</script>

	<script src="lib/dojo/dojo.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="lib/dojo/tt-rss-layer.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/tt-rss.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/common.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>

	<script type="text/javascript">
		require({cache:{}});
	</script>

	<script type="text/javascript">
	try {
					/* global Plugins, xhr, __ */

Plugins.Psql_Trgm = {
	showRelated: function (id) {
		const dialog = new dijit.Dialog({
			title: __("Related articles"),
			content: __("Loading, please wait...")
		});

		const tmph = dojo.connect(dialog, "onShow", null, function (/* e */) {
			dojo.disconnect(tmph);

			xhr.post("backend.php", {op: 'PluginHandler', plugin: 'af_psql_trgm', method: 'showrelated', id: id}, (reply) => {
				dialog.attr('content', reply);
			});
		});

		dialog.show();
	}
};


				} catch (e) {
					console.warn('failed to initialize plugin JS: af_psql_trgm', e);
				}try {
					/* global require, Plugins, PluginHost, xhr, App, Notify, fox, __ */

require(['dojo/_base/kernel', 'dojo/ready'], function  (dojo, ready) {
	ready(function() {

		Plugins.Note = {
			set_click_handler: function() {
				document.querySelectorAll('.article-note[data-note-for]').forEach((note) => {
					note.onclick = function() {
						Plugins.Note.edit(this.getAttribute('data-note-for'));
					}
				});
			},
			edit: function(id) {
				const dialog = new fox.SingleUseDialog({
					title: __("Edit article note"),
					execute: function () {
						if (this.validate()) {
							Notify.progress("Saving article note...", true);

							xhr.json("backend.php", this.attr('value'), (reply) => {
								Notify.close();
								dialog.hide();

								if (reply) {
									document.querySelectorAll(`div[data-note-for="${reply.id}"]`).forEach((elem) => {
										elem.querySelector('.body').innerHTML = reply.note;
										reply.note ? elem.show() : elem.hide();
									});
								}
							});
						}
					},
					content: __("Loading, please wait...")
				});

				const tmph = dojo.connect(dialog, 'onShow', function () {
					dojo.disconnect(tmph);

					xhr.post("backend.php", App.getPhArgs("note", "edit", {id: id}), (reply) => {
						dialog.attr('content', reply);
					});
				});

				dialog.show();
			}
		};

		PluginHost.register(PluginHost.HOOK_HEADLINES_RENDERED, () => {
			Plugins.Note.set_click_handler();
		});
	});
});

				} catch (e) {
					console.warn('failed to initialize plugin JS: note', e);
				}try {
					/* global Plugins, App, Notify, fox, xhr, __ */

Plugins.Share = {
	shareArticle: function(id) {
		const dialog = new fox.SingleUseDialog({
			id: "shareArticleDlg",
			title: __("Share article by URL"),
			newurl: function () {
				if (confirm(__("Generate new share URL for this article?"))) {

					Notify.progress("Trying to change URL...", true);

					xhr.json("backend.php", App.getPhArgs("share", "newkey", {id: id}), (reply) => {
						if (reply) {
							const new_link = reply.link;
							const target = dialog.domNode.querySelector(".target-url");

							if (new_link && target) {

								target.innerHTML = target.innerHTML.replace(/&amp;key=.*$/,
									"&amp;key=" + new_link);

								target.href = target.href.replace(/&key=.*$/,
									"&key=" + new_link);

								document.querySelector('.share-icon-' + id)?.classList.add('is-shared');

								Notify.close();

							} else {
								Notify.error("Could not change URL.");
							}
						}
					});
				}

			},
			unshare: function () {
				if (confirm(__("Remove sharing for this article?"))) {
					xhr.post("backend.php", App.getPhArgs("share", "unshare", {id: id}), (reply) => {
						Notify.info(reply);
						document.querySelector('.share-icon-' + id)?.classList.remove('is-shared');
						dialog.hide();
					});
				}

			},
			content: __("Loading, please wait...")
		});

		const tmph = dojo.connect(dialog, 'onShow', function () {
			dojo.disconnect(tmph);

			xhr.post("backend.php", App.getPhArgs("share", "shareDialog", {id: id}), (reply) => {
				dialog.attr('content', reply)
				document.querySelector('.share-icon-' + id)?.classList.add('is-shared');
			});
		});

		dialog.show();
	}
}

				} catch (e) {
					console.warn('failed to initialize plugin JS: share', e);
				}	</script>

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
		<link href="themes/light.css?__SCRIPTS_TS__" rel="stylesheet" type="text/css" data-orig-href="themes/light.css"/>

		<style type="text/css">
			body.css_loading noscript {
				display : block;
				margin : 16px;
			}
		</style>
	</noscript>

	<meta http-equiv="Content-Type" content="text/html; charset=utf-8"/>
	<meta name="referrer" content="no-referrer"/>
</head>

<body class="flat ttrss_main ttrss_index css_loading">

<noscript class="alert alert-error">Javascript is disabled. Please enable it.</noscript>

<div id="overlay">
	<div id="overlay_inner">
		Loading, please wait...		<div dojoType="dijit.ProgressBar" places="0" style="width : 300px" id="loading_bar"
	     progress="0" maximum="100">
		</div>
	</div>
</div>

<div id="notify" class="notify"></div>
<div id="cmdline" style="display : none"></div>

<div id="main" dojoType="dijit.layout.BorderContainer">
    <!-- Dims the page behind the feed drawer in narrow/phone layout; tap to close.
         Must live inside #main (which is a stacking context) so it stacks below
         the drawer rather than above it. BorderContainer ignores non-widget children. -->
    <div id="feeds-holder-backdrop" onclick="App.toggleSidebar(false)"></div>
    <div id="feeds-holder" dojoType="dijit.layout.ContentPane" region="leading" style="width : 20%" splitter="true">
        <div id="feedlistLoading" class="text-center text-muted text-small">
			  	<img class="icon-three-dots" src="images/three-dots.svg?2">
				Loading, please wait...			</div>
                <div id="feedTree"></div>
    </div>

    <div dojoType="dijit.layout.BorderContainer" region="center" id="content-wrap">
        <div id="toolbar-frame" dojoType="dijit.layout.ContentPane" region="top">
            <div id="toolbar" dojoType="fox.Toolbar">

					<!-- order -1: feed drawer toggle (the app logo), shown only in narrow/phone layout -->
					<img class="sidebar-toggle" src="images/favicon-72px.png" alt="" draggable="false" role="button" tabindex="0"
						aria-expanded="false" title="Toggle feed list"
						onclick="App.toggleSidebar()"
						onkeydown="if (event.key === 'Enter' || event.key === ' ') { App.toggleSidebar(); event.preventDefault(); }">

			 	<!-- order 0, default -->

				
				<!-- order 5: alert icons -->

						<i class="material-icons net-alert" style="display : none; order : 5"
							title="Communication problem with server.">error_outline</i>

						<i class="material-icons log-alert" style="display : none; order : 5" onclick="App.openPreferences('system')"
							title="Recent entries found in event log.">warning</i>

						<a id="updates-available" target="_blank" rel="noopener noreferrer" href="" style="display: none; order: 5">
							<i class="material-icons icon-new-version" title="Updates for Tiny Tiny RSS are available.">new_releases</i>
						</a>

						<a id="plugin-updates-available" href="prefs.php?tab=prefs" style="display: none; order: 5">
							<i class="material-icons icon-new-version" title="Updates for some local plugins are available.">new_releases</i>
						</a>

				<!-- order 10: headlines toolbar -->
				<div id="toolbar-headlines" dojoType="fox.Toolbar" style="order : 10"> </div>

				<!-- order 20: main toolbar contents (dropdowns) -->
				<form id="toolbar-main" dojoType="dijit.form.Form" action="" style="order : 20" onsubmit="return false">
					<select name="view_mode" title="Show articles"
						onchange="Feeds.onViewModeChanged()"
						dojoType="fox.form.Select">
						<option selected="selected" value="adaptive">Adaptive</option>
						<option value="all_articles">All Articles</option>
						<option value="marked">Starred</option>
						<option value="published">Published</option>
						<option value="unread">Unread</option>
						<option value="has_note">With Note</option>
					</select>

					<select title="Sort articles"
							onchange="Feeds.onViewModeChanged()"
							dojoType="fox.form.Select" name="order_by">

						<option selected="selected" value="default">Default</option>
							<option value="feed_dates">Newest first</option>
							<option value="date_reverse">Oldest first</option>
							<option value="title">Title</option>

											</select>

					<select class="catchup-button" id="main-catchup-dropdown" dojoType="fox.form.Select"
						data-prevent-value-change="true">
						<option value="">Mark as read</option>
						<option value="1day">Older than one day</option>
						<option value="1week">Older than one week</option>
						<option value="2week">Older than two weeks</option>
					</select>
				</form>

				<!-- toolbar actions dropdown: order 30 -->

				<div class="action-chooser" style="order : 30">
					
					<div dojoType="fox.form.DropDownButton" class="action-button" title="Actions...">
						<span><i class="material-icons">menu</i></span>
						<div dojoType="dijit.Menu" style="display: none">
								<script type='dojo/method' event='onOpen' args='evt,a,b,c'>
									const widescreen = this.getChildren().find((m) => m.id == 'qmcToggleWidescreen');
									const expanded = this.getChildren().find((m) => m.id == 'qmcToggleExpanded');
									const combined = this.getChildren().find((m) => m.id == 'qmcToggleCombined');

									if (combined)
										combined.attr('label',
											App.isCombinedMode() ? "Switch to three panel view" : "Switch to combined view");

									if (widescreen)
										widescreen
											.attr('hidden', !!App.isCombinedMode())
											.attr('label',
												App.isWideScreenMode() ? "Disable widescreen mode" : "Enable widescreen mode");

									if (expanded)
										expanded
											.attr('hidden', !App.isCombinedMode())
											.attr('label',
												App.isExpandedMode() ? "Expand selected article only" : "Expand all articles");
								</script>

								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcPrefs')">Preferences...</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcSearch')">Search...</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcFilterFeeds')">Search feeds...</div>
								<div dojoType="dijit.MenuItem" disabled="1">Feed actions:</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcAddFeed')">Subscribe to feed...</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcEditFeed')">Edit this feed...</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcRemoveFeed')">Unsubscribe</div>
								<div dojoType="dijit.MenuItem" disabled="1">All feeds:</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcCatchupAll')">Mark as read</div>
								<div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcShowOnlyUnread')">(Un)hide read feeds</div>
								<div dojoType="dijit.MenuItem" disabled="1">UI layout:</div>
								<div dojoType="dijit.MenuItem" id="qmcToggleCombined" onclick="App.onActionSelected('qmcToggleCombined')">Toggle combined mode</div>
								<div dojoType="dijit.MenuItem" id="qmcToggleWidescreen" onclick="App.onActionSelected('qmcToggleWidescreen')">
									Toggle widescreen mode</div>
								<div dojoType="dijit.MenuItem" id="qmcToggleExpanded" onclick="App.onActionSelected('qmcToggleExpanded')">
									Toggle expand all articles</div>
								<div dojoType="dijit.MenuItem" disabled="1">Other actions:</div>
                        <div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcHKhelp')">Keyboard shortcuts help</div>

                        
                                                    <div dojoType="dijit.MenuItem" onclick="App.onActionSelected('qmcLogout')">Logout</div>
                                            </div>
                </div>
            </div>
        </div> <!-- toolbar -->
        </div> <!-- toolbar pane -->
        <div id="headlines-wrap-inner" dojoType="dijit.layout.BorderContainer" region="center">
            <div id="headlines-frame" dojoType="dijit.layout.ContentPane" tabindex="0"
                    region="center">
                <div id="headlinesInnerContainer">
                    <div class="whiteBox">Loading, please wait...</div>
                </div>
            </div>
            <div id="content-insert" dojoType="dijit.layout.ContentPane" region="bottom"
                style="height : 50%" splitter="true"></div>
        </div>
    </div>
</div>

</body>
</html>
