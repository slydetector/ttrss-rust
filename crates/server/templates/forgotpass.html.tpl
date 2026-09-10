<!DOCTYPE html>
<html>
<head>
	<title>Tiny Tiny RSS</title>
	<link rel="shortcut icon" type="image/png" href="images/favicon.png">
	<link rel="icon" type="image/png" sizes="72x72" href="images/favicon-72px.png">
	<meta http-equiv="Content-Type" content="text/html; charset=utf-8">
<script src="lib/dojo/dojo.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="lib/dojo/tt-rss-layer.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/common.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
<script src="js/utility.js?__SCRIPTS_TS__" type="text/javascript" charset="utf-8"></script>
</head>
<body class="flat ttrss_utility css_loading">
<div class="container">
<script type="text/javascript">
	const __default_light_theme = "__THEME_LIGHT__";
	const __default_dark_theme = "__THEME_DARK__";
</script>

<script type="text/javascript">
	require(['dojo/parser', "dojo/ready", 'dijit/form/Button','dijit/form/CheckBox', 'dijit/form/Form',
		'dijit/form/Select','dijit/form/TextBox','dijit/form/ValidationTextBox'],function(parser, ready){
		ready(function() {
			parser.parse();
		});
	});
</script>

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

<h1>Password recovery</h1>
<div class="content">
__CONTENT__
</div>
</div>
</body>
</html>
