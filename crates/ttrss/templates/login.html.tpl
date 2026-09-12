<!DOCTYPE html>
<html>
<head>
	<title>Tiny Tiny RSS : Login</title>
	<link rel="shortcut icon" type="image/png" href="images/favicon.png">
	<meta http-equiv="Content-Type" content="text/html; charset=utf-8">
	<meta name="viewport" content="initial-scale=1,width=device-width" />
<script src="lib/dojo/dojo.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="lib/dojo/tt-rss-layer.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/common.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/utility.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>

	
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

	<script type="text/javascript">
		require({cache:{}});
	</script>

	<script type="text/javascript">
		const __default_light_theme = "__THEME_LIGHT__";
		const __default_dark_theme = "__THEME_DARK__";
	</script>

	<script type="text/javascript">
	/* exported Plugins */
	const Plugins = {};

		</script>
</head>

<body class="flat ttrss_utility ttrss_login css_loading">

<script type="text/javascript">
	const UtilityApp = {
        previousLogin: "",
	    init: function() { /* invoked by UtilityJS */
            require(['dojo/parser', "dojo/ready", 'dijit/form/Button','dijit/form/CheckBox', 'dijit/form/Form',
                'dijit/form/Select','dijit/form/TextBox','dijit/form/ValidationTextBox'],function(parser, ready){
                ready(function() {
					parser.parse();

					dijit.byId("bw_limit").attr("checked", Cookie.get("ttrss_bwlimit") == 'true');
					dijit.byId("login").focus();
                });
            });
		},
        fetchProfiles: function() {
	        const login = dijit.byId("login").attr('value');

	        if (login && login != this.previousLogin) {
                this.previousLogin = login;

                xhr.json("public.php", {op: "getprofiles", login: login},
                    (reply) => {
                        const profile = dijit.byId('profile');

                        profile.removeOption(profile.getOptions());

                        reply.forEach((p) => {
                            profile
                                .attr("disabled", false)
                                .addOption(p);
                        });
                    });
            }
	    },
        gotoRegForm: function() {
        	window.location.href = "register.php";
        	return false;
    	},
        bwLimitChange: function(elem) {
        	Cookie.set("ttrss_bwlimit", elem.checked,
				__LIFETIME__);
	    }
    };


</script>


<div class="container">

	<h1>Authentication</h1>
	<div class="content">
		<form action="public.php?return=__RETURN_URL__"
			  dojoType="dijit.form.Form" method="POST">

			<input dojoType="dijit.form.TextBox" style="display : none"
                name="op"
               value="login">
			
			<fieldset>
				<label>Login:</label>
				<input name="login" id="login" dojoType="dijit.form.TextBox" type="text"
					autocomplete="username" autocapitalize="none" autocorrect="off" spellcheck="false"
					onchange="UtilityApp.fetchProfiles()"
					onfocus="UtilityApp.fetchProfiles()"
					onblur="UtilityApp.fetchProfiles()"
										required="1" value="" />
			</fieldset>

			<fieldset>
				<label>Password:</label>

				<input type="password" name="password" required="1"
					autocomplete="current-password"
					dojoType="dijit.form.TextBox"
					class="input input-text"
					onchange="UtilityApp.fetchProfiles()"
					onfocus="UtilityApp.fetchProfiles()"
					onblur="UtilityApp.fetchProfiles()"
										value=""/>
			</fieldset>
							<fieldset class="align-right">
					<a href="public.php?op=forgotpass">I forgot my password</a>
				</fieldset>
			
							<fieldset>
					<label>Profile:</label>

					<select disabled='disabled' name="profile" id="profile" dojoType='dijit.form.Select'>
						<option>Default profile</option>
					</select>
				</fieldset>

				<fieldset class="narrow">
					<label> </label>

					<label id="bw_limit_label">
						<input dojoType='dijit.form.CheckBox' name="bw_limit"
                    onchange="UtilityApp.bwLimitChange(this)" id="bw_limit">						Use less traffic</label>
				</fieldset>

				<div dojoType="dijit.Tooltip" connectId="bw_limit_label" position="below" style="display:none">
					Does not display images in articles, reduces automatic refreshes.				</div>

				<fieldset class="narrow">
					<label> </label>

					<label id="safe_mode_label">
						<input dojoType='dijit.form.CheckBox' name="safe_mode"
                     id="">						Safe mode					</label>
				</fieldset>

				<div dojoType="dijit.Tooltip" connectId="safe_mode_label" position="below" style="display:none">
					Uses default theme and prevents all plugins from loading.				</div>

									<fieldset class="narrow">
						<label> </label>
						<label>
							<input dojoType='dijit.form.CheckBox' name="remember_me"
                     id="">							Remember me						</label>
					</fieldset>
							
			<hr/>

			<fieldset class="align-right">
				<label> </label>
								<button dojoType="dijit.form.Button" class="alt-primary" type="submit">Log in</button>											</fieldset>

		</form>
	</div>

	<div class="footer">
		<a href="https://github.com/tt-rss/tt-rss">Tiny Tiny RSS</a>
	</div>

</div>

</body>
</html>
