use std::collections::BTreeMap;

use super::*;

/// A JSON `\u` escape of `hex`, built so no escape is written literally
/// in this file
fn u(hex: &str) -> String {
    format!("{}u{hex}", '\\')
}

/// Strings an attacker would title a post with
fn hostile() -> Vec<String> {
    vec![
        "</script><script>alert(1)</script>".into(),
        "</SCRIPT >".into(),
        "<!-- <script>".into(),
        "]]><![CDATA[".into(),
        "&lt;b&gt; &amp; &#60;".into(),
        "line\u{2028}sep\u{2029}para".into(),
        // A literal backslash-u sequence is text, not an escape to undo
        format!(r#""quoted" 'single' \backslash {} literal"#, u("003c")),
        "<img src=x onerror=alert(1)>".into(),
        "\0 nul and \u{1b} escape".into(),
        "emoji 🦀 and 𝕏 outside the BMP".into(),
    ]
}

fn assert_inert(text: &str) {
    for bad in ['<', '>', '&', '\u{2028}', '\u{2029}'] {
        assert!(!text.contains(bad), "{bad:?} in {text}");
    }
}

#[test]
fn the_text_has_no_markup_and_reads_back() {
    for hostile in hostile() {
        let props = vec![hostile];
        let text = encode(&props).unwrap();
        assert_inert(&text);
        let back: Vec<String> = from_script_text(&text).unwrap();
        assert_eq!(back, props, "{text}");
    }
}

#[test]
fn the_escapes_are_json_unicode_escapes() {
    let text = encode(&"<>&\u{2028}\u{2029}").unwrap();
    let want: String = ["003c", "003e", "0026", "2028", "2029"].map(u).concat();
    assert!(text.contains(&want), "{text}");
}

/// Map keys are strings in the JSON too, and every C0 control is escaped
#[test]
fn hostile_keys_and_controls_are_inert() {
    let mut map = BTreeMap::new();
    map.insert(
        "</script><!--<script>".to_string(),
        "v\r\n\0\u{7f}\u{85}".to_string(),
    );
    map.insert("k&amp;".to_string(), "<!--".to_string());
    let text = encode(&map).unwrap();
    assert_inert(&text);
    assert!(!text.contains(['\r', '\n', '\0']), "{text:?}");
    let back: BTreeMap<String, String> = from_script_text(&text).unwrap();
    assert_eq!(back, map);

    let controls: String = (0u8..0x20).map(char::from).collect();
    let text = encode(&controls).unwrap();
    assert!(text.bytes().all(|b| b >= 0x20), "{text:?}");
    assert_eq!(from_script_text::<String>(&text).unwrap(), controls);
}

#[test]
fn the_element_holds_only_the_escaped_text() {
    let props = hostile().concat();
    let element = test_element(DATA_ID, &props).unwrap();
    let open = r#"<script type="application/json" id="agora-app-data">"#;
    let inner = element
        .strip_prefix(open)
        .and_then(|rest| rest.strip_suffix("</script>"))
        .unwrap();
    assert_inert(inner);
    assert_eq!(from_script_text::<String>(inner).unwrap(), props);
}

#[test]
fn another_island_has_its_own_id() {
    let element = test_element("agora-thread-data", &1u8).unwrap();
    assert!(
        element.starts_with(
            r#"<script type="application/json" id="agora-thread-data">"#
        ),
        "{element}"
    );
}

#[test]
#[should_panic(expected = "lowercase letters, digits and hyphens")]
fn an_id_that_could_break_the_tag_is_refused() {
    let _ = test_element(r#"x"><script>"#, &1u8);
}

#[test]
fn a_very_long_string_round_trips() {
    let props = "</script>\u{2028}&".repeat(100_000);
    let text = encode(&props).unwrap();
    assert_inert(&text);
    assert_eq!(from_script_text::<String>(&text).unwrap(), props);
}

/// No Rust string holds a lone surrogate, so the server never writes one;
/// a page that does is refused, not read as something else
#[test]
fn a_lone_surrogate_is_refused() {
    for escape in [
        u("d800"),
        u("dfff"),
        u("d800") + &u("0041"),
        u("dc00") + &u("d800"),
    ] {
        let text = format!(
            r#"{{"agentkit":"{AGENTKIT_VERSION}","props":"{escape}"}}"#
        );
        assert!(
            matches!(
                from_script_text::<String>(&text),
                Err(IslandError::Json(_))
            ),
            "{text}"
        );
    }
    // A pair is one character
    let pair = u("d83e") + &u("dd80");
    let text =
        format!(r#"{{"agentkit":"{AGENTKIT_VERSION}","props":"{pair}"}}"#);
    assert_eq!(from_script_text::<String>(&text).unwrap(), "🦀");
}

#[test]
fn another_agentkits_page_is_refused() {
    let text = r#"{"agentkit":"0.0.1","props":"x"}"#;
    match from_script_text::<String>(text) {
        Err(IslandError::Version { page, client }) => {
            assert_eq!(page, "0.0.1");
            assert_eq!(client, AGENTKIT_VERSION);
        }
        other => panic!("{other:?}"),
    }
    // A patch release apart is another agentkit too
    let mut parts: Vec<u64> = AGENTKIT_VERSION
        .split('.')
        .map(|p| p.parse().unwrap())
        .collect();
    parts[2] += 1;
    let other = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
    let text = encode(&1u8).unwrap().replace(AGENTKIT_VERSION, &other);
    assert!(matches!(
        from_script_text::<u8>(&text),
        Err(IslandError::Version { .. })
    ));
}

/// A newer page whose props changed shape is told apart by its version,
/// not reported as unreadable
#[test]
fn the_version_is_read_before_the_props() {
    let text = r#"{"agentkit":"9.9.9","props":{"new":"shape"}}"#;
    match from_script_text::<u8>(text) {
        Err(IslandError::Version { page, .. }) => assert_eq!(page, "9.9.9"),
        other => panic!("{other:?}"),
    }
    // An escaped version string is still read
    let text = format!(r#"{{"agentkit":"9.9.{}","props":{{}}}}"#, u("0039"));
    match from_script_text::<u8>(&text) {
        Err(IslandError::Version { page, .. }) => assert_eq!(page, "9.9.9"),
        other => panic!("{other:?}"),
    }
    // This version with props of another shape is unreadable
    let text = format!(
        r#"{{"agentkit":"{AGENTKIT_VERSION}","props":{{"new":"shape"}}}}"#
    );
    assert!(matches!(
        from_script_text::<u8>(&text),
        Err(IslandError::Json(_))
    ));
    // No stamp at all is unreadable
    assert!(matches!(
        from_script_text::<u8>(r#"{"props":1}"#),
        Err(IslandError::Json(_))
    ));
}

/// What a browser's `textContent` might add around the block, or a page
/// might add inside it
#[test]
fn surrounding_whitespace_reads_and_trailing_text_does_not() {
    let text = encode(&1u8).unwrap();
    assert_eq!(from_script_text::<u8>(&format!("\n  {text}\n")).unwrap(), 1);
    assert!(from_script_text::<u8>(&format!("{text} trailing")).is_err());
    let doubled =
        format!(r#"{{"agentkit":"{AGENTKIT_VERSION}","props":1,"props":2}}"#);
    assert!(from_script_text::<u8>(&doubled).is_err());
}

#[test]
fn the_shape_is_version_and_props() {
    let text = encode(&1u8).unwrap();
    assert_eq!(
        text,
        format!(r#"{{"agentkit":"{AGENTKIT_VERSION}","props":1}}"#)
    );
}

/// [`script_element`] for any value, as the tests use it
fn test_element<P: Serialize>(
    id: &'static str,
    props: &P,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    #[serde(transparent)]
    struct Any<'a, P: Serialize>(&'a P);
    impl<P: Serialize> Clone for Any<'_, P> {
        fn clone(&self) -> Self {
            Any(self.0)
        }
    }
    impl<P: Serialize> Anonymous for Any<'_, P> {
        fn anonymous(self) -> Self {
            self
        }
    }
    script_element(id, &Any(props).public())
}
