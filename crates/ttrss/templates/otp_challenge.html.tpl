<!DOCTYPE html>
<html>
<head>
    <title>Tiny Tiny RSS</title>
    <meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
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

        const UtilityApp = {
            init: function() {
                require(['dojo/parser', "dojo/ready", 'dijit/form/Button', 'dijit/form/Form',
                        'dijit/form/TextBox','dijit/form/ValidationTextBox'],function(parser, ready){
                    ready(function() {
                        parser.parse();
                        dijit.byId("otp").focus();
                    });
                });
            },
        };
    </script>
</head>
<body class="flat ttrss_utility otp css_loading">
    <h1>Authentication</h1>
    <div class="content">
        <form dojoType="dijit.form.Form" action="public.php?return=__RETURN_URL__" method="post" class="otpform">
            <input dojoType="dijit.form.TextBox" style="display : none" name="login" value="__LOGIN__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="password" value="__PASSWORD__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="bw_limit" value="__BW_LIMIT__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="safe_mode" value="__SAFE_MODE__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="remember_me" value="__REMEMBER_ME__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="profile" value="__PROFILE__">
            <input dojoType="dijit.form.TextBox" style="display : none" name="op" value="login">
            <fieldset>
                <label>Please enter verification code (OTP):</label>
                <input id="otp" dojoType="dijit.form.ValidationTextBox" required="1" autocomplete="off" size="6" name="otp" value="">
                <button dojoType="dijit.form.Button" class="alt-primary" type="submit">Continue</button>
            </fieldset>
        </form>
    </div>
</body>
</html>
