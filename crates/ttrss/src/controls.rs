//! Port of the `Controls` form-tag helpers (`tt-rss/include/controls.php`) used
//! by the read-side feed forms (the `editfeeds` batch-edit dialog).
//!
//! Each helper mirrors the PHP output byte-for-byte (verified against the
//! running oracle). Attribute lists are ordered (PHP arrays preserve insertion
//! order), represented as `&[(&str, String)]`.

use crate::util;

/// PHP `attributes_to_string($attributes)`: `k="v"` pairs joined by single
/// spaces; a falsy `disabled` attribute is dropped (the PHP's special handling).
pub fn attributes_to_string(attributes: &[(&str, String)]) -> String {
    let mut parts = Vec::new();
    for (k, v) in attributes {
        if *k == "disabled" && !crate::core::config::sql_bool_to_bool(v) {
            continue;
        }
        parts.push(format!("{}=\"{}\"", k, util::htmlspecialchars(v)));
    }
    parts.join(" ")
}

/// PHP `Controls::hidden_tag($name, $value, $attributes = [])`.
pub fn hidden_tag(name: &str, value: &str) -> String {
    format!(
        "<input dojoType=\"dijit.form.TextBox\" style=\"display : none\"\n                name=\"{}\"\n               value=\"{}\">",
        util::htmlspecialchars(name),
        util::htmlspecialchars(value)
    )
}

/// PHP `Controls::checkbox_tag($name, $checked, $value, $attributes, $id)`.
pub fn checkbox_tag(
    name: &str,
    checked: bool,
    value: &str,
    attributes: &[(&str, String)],
) -> String {
    let is_checked = if checked { "checked" } else { "" };
    let value_str = if !value.is_empty() {
        format!("value=\"{}\"", util::htmlspecialchars(value))
    } else {
        String::new()
    };
    let attrs = attributes_to_string(attributes);
    format!(
        "<input dojoType='dijit.form.CheckBox' name=\"{}\"\n                  {} {} {} id=\"{}\">",
        util::htmlspecialchars(name),
        value_str,
        is_checked,
        attrs,
        ""
    )
}

/// PHP `Controls::select_tag($name, $value, $values, $attributes, $id)`: a
/// `<select>` from a flat list of string values (each is both value and label).
pub fn select_tag(
    name: &str,
    value: &str,
    values: &[String],
    attributes: &[(&str, String)],
) -> String {
    let attrs_str = attributes_to_string(attributes);
    let dojo_type = if attrs_str.contains("dojoType") {
        ""
    } else {
        "dojoType='fox.form.Select' "
    };
    let mut rv = format!(
        "<select {}name=\"{}\"\n         id=\"\" name=\"{}\" {}>",
        dojo_type,
        util::htmlspecialchars(name),
        util::htmlspecialchars(name),
        attrs_str
    );
    for v in values {
        let is_sel = if v == value {
            "selected=\"selected\""
        } else {
            ""
        };
        rv.push_str(&format!(
            "<option value=\"{}\" {}>{}</option>",
            util::htmlspecialchars(v),
            is_sel,
            util::htmlspecialchars(v)
        ));
    }
    rv.push_str("</select>");
    rv
}

/// PHP `Controls::select_hash($name, $value, $values, $attributes, $id)`: a
/// `<select>` from an ordered key→label map.
pub fn select_hash(
    name: &str,
    value: &str,
    values: &[(String, String)],
    attributes: &[(&str, String)],
) -> String {
    let attrs_str = attributes_to_string(attributes);
    let dojo_type = if attrs_str.contains("dojoType") {
        ""
    } else {
        "dojoType='fox.form.Select' "
    };
    let mut rv = format!(
        "<select {}name=\"{}\"\n         id=\"\" name=\"{}\" {}>",
        dojo_type,
        util::htmlspecialchars(name),
        util::htmlspecialchars(name),
        attrs_str
    );
    for (k, v) in values {
        let is_sel = if k == value {
            "selected=\"selected\""
        } else {
            ""
        };
        rv.push_str(&format!(
            "<option value=\"{}\" {}>{}</option>",
            util::htmlspecialchars(k),
            is_sel,
            util::htmlspecialchars(v)
        ));
    }
    rv.push_str("</select>");
    rv
}

/// PHP `Controls::icon($icon, $attributes = [])`.
pub fn icon(icon: &str) -> String {
    format!("<i class=\"material-icons\">{}</i>", icon)
}

/// PHP `format_notice($msg)` (`tt-rss/include/controls_compat.php`).
pub fn format_notice(msg: &str) -> String {
    format!("<div class=\"alert alert-info\" id=\"\">{}</div>", msg)
}

/// PHP `format_warning($msg)` (`tt-rss/include/controls_compat.php`).
pub fn format_warning(msg: &str) -> String {
    format!("<div class=\"alert\" id=\"\">{}</div>", msg)
}

/// PHP `format_error($msg)` (`tt-rss/include/controls_compat.php`, `$id = ""` default).
pub fn format_error(msg: &str) -> String {
    crate::util::format_error(msg, "")
}

/// PHP `Controls::input_tag($name, $value, $type = "text", $attributes = [], $id = "")`.
pub fn input_tag(
    name: &str,
    value: &str,
    type_: &str,
    attributes: &[(&str, String)],
    id: &str,
) -> String {
    let attrs_str = attributes_to_string(attributes);
    let dojo_type = if attrs_str.contains("dojoType") {
        ""
    } else {
        "dojoType='dijit.form.TextBox'"
    };
    format!(
        "<input name=\"{}\" {} {} id=\"{}\"\n   type=\"{}\" value=\"{}\">",
        util::htmlspecialchars(name),
        dojo_type,
        attrs_str,
        util::htmlspecialchars(id),
        type_,
        util::htmlspecialchars(value)
    )
}

/// PHP `Controls::number_spinner_tag($name, $value, $attributes = [], $id = "")`.
pub fn number_spinner_tag(
    name: &str,
    value: &str,
    attributes: &[(&str, String)],
    id: &str,
) -> String {
    // `['dojoType' => 'dijit.form.NumberSpinner', ...$attributes]`.
    let mut merged: Vec<(&str, String)> =
        vec![("dojoType", "dijit.form.NumberSpinner".to_string())];
    merged.extend(
        attributes
            .iter()
            .filter(|(k, _)| *k != "dojoType")
            .map(|(k, v)| (*k, v.clone())),
    );
    input_tag(name, value, "text", &merged, id)
}

/// PHP `Controls::button_tag($value, $type, $attributes)`.
pub fn button_tag(value: &str, type_: &str, attributes: &[(&str, String)]) -> String {
    format!(
        "<button dojoType=\"dijit.form.Button\" {} type=\"{}\">{}</button>",
        attributes_to_string(attributes),
        type_,
        value
    )
}

/// PHP `Controls::submit_tag($value, $attributes = [])`.
pub fn submit_tag(value: &str) -> String {
    button_tag(value, "submit", &[("class", "alt-primary".to_string())])
}

/// PHP `Controls::cancel_dialog_tag($value, $attributes = [])`.
pub fn cancel_dialog_tag(value: &str) -> String {
    button_tag(
        value,
        "",
        &[("onclick", "App.dialogOf(this).hide()".to_string())],
    )
}
/// PHP `Controls::pluginhandler_tags($plugin, $method)`: the hidden
/// `op`/`plugin`/`method` tags a printed plugin-method form carries
/// (concatenated, no separator; the plugin class name lowercase).
pub fn pluginhandler_tags(plugin_class: &str, method: &str) -> String {
    [
        hidden_tag("op", "PluginHandler"),
        hidden_tag("plugin", &plugin_class.to_lowercase()),
        hidden_tag("method", method),
    ]
    .concat()
}

