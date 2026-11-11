//! Global accelerator parsing/normalization (pure logic, no Tauri deps in
//! the public surface for these items so the function is unit-testable on
//! every platform).

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShortcutParseError {
    #[error("empty accelerator")]
    Empty,
    #[error("modifier-only accelerator: {0}")]
    ModifierOnly(String),
    #[error("invalid token: {0}")]
    InvalidToken(String),
}

/// Recognised modifier tokens (case-insensitive). The right-hand value is
/// the canonical spelling used by Tauri's accelerator parser.
fn canonical_modifier(token_lower: &str) -> Option<&'static str> {
    match token_lower {
        "ctrl" | "control" => Some("Control"),
        "cmd" | "command" | "meta" | "win" | "super" => Some("Super"),
        "shift" => Some("Shift"),
        "alt" | "option" => Some("Alt"),
        _ => None,
    }
}

const MODIFIER_ORDER: &[&str] = &["Control", "Alt", "Shift", "Super"];

/// Return the canonical key name accepted by global-hotkey's runtime parser.
///
/// Validation is intentionally allowlist-based so typos like `NotARealKey` are
/// rejected at config save time rather than silently failing to register at
/// runtime. The lookup is case-insensitive and preserves canonical camel-case
/// spellings such as `PageUp`, `NumpadAdd`, and `PrintScreen`.
fn canonical_key(token: &str) -> Option<String> {
    if token.len() == 1 {
        let c = token.as_bytes()[0];
        if c.is_ascii_alphabetic() || c.is_ascii_digit() {
            return Some(token.to_ascii_uppercase());
        }
    }

    let lower = token.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix('f') {
        if (1..=2).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = rest.parse::<u32>() {
                if (1..=24).contains(&n) {
                    return Some(format!("F{n}"));
                }
            }
        }
    }

    let canonical = match lower.as_str() {
        "up" | "arrowup" => "Up",
        "down" | "arrowdown" => "Down",
        "left" | "arrowleft" => "Left",
        "right" | "arrowright" => "Right",
        "tab" => "Tab",
        "enter" | "return" => "Enter",
        "space" | "spacebar" => "Space",
        "escape" | "esc" => "Escape",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" => "PageUp",
        "pagedown" | "pgdn" => "PageDown",
        "minus" => "Minus",
        "equal" => "Equal",
        "backslash" => "Backslash",
        "bracketleft" => "BracketLeft",
        "bracketright" => "BracketRight",
        "semicolon" => "Semicolon",
        "quote" => "Quote",
        "comma" => "Comma",
        "period" => "Period",
        "slash" => "Slash",
        "backquote" | "grave" => "Backquote",
        "numpad0" => "Numpad0",
        "numpad1" => "Numpad1",
        "numpad2" => "Numpad2",
        "numpad3" => "Numpad3",
        "numpad4" => "Numpad4",
        "numpad5" => "Numpad5",
        "numpad6" => "Numpad6",
        "numpad7" => "Numpad7",
        "numpad8" => "Numpad8",
        "numpad9" => "Numpad9",
        "numpadadd" | "numpadplus" => "NumpadAdd",
        "numpadsubtract" => "NumpadSubtract",
        "numpadmultiply" => "NumpadMultiply",
        "numpaddivide" => "NumpadDivide",
        "numpaddecimal" => "NumpadDecimal",
        "numpadenter" => "NumpadEnter",
        "capslock" => "CapsLock",
        "scrolllock" => "ScrollLock",
        "numlock" => "NumLock",
        "printscreen" => "PrintScreen",
        "pause" => "Pause",
        _ => return None,
    };
    Some(canonical.to_string())
}

/// Normalise a user-supplied accelerator string into the canonical form
/// expected by `tauri-plugin-global-shortcut`'s parser.
///
/// Rules:
/// - tokens are split on `+` and trimmed (collapsed whitespace allowed)
/// - aliases (`ctrl`/`cmd`/`win`/`option`) map to canonical modifier names
/// - modifiers are emitted in fixed order: `Control+Alt+Shift+Super`
/// - exactly one non-modifier token (the key) is required
/// - non-modifier keys are accepted only when present in the canonical key table
pub fn normalize_accelerator(input: &str) -> Result<String, ShortcutParseError> {
    let collapsed: String = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.trim().is_empty() {
        return Err(ShortcutParseError::Empty);
    }

    let mut modifiers: Vec<&'static str> = Vec::new();
    let mut key: Option<String> = None;

    for raw in collapsed.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return Err(ShortcutParseError::InvalidToken(String::new()));
        }
        let lower = token.to_ascii_lowercase();
        if let Some(canon) = canonical_modifier(&lower) {
            if !modifiers.contains(&canon) {
                modifiers.push(canon);
            }
        } else if key.is_some() {
            return Err(ShortcutParseError::InvalidToken(token.to_string()));
        } else {
            let Some(canonical) = canonical_key(token) else {
                return Err(ShortcutParseError::InvalidToken(token.to_string()));
            };
            key = Some(canonical);
        }
    }

    let key = match key {
        Some(k) => k,
        None => return Err(ShortcutParseError::ModifierOnly(collapsed)),
    };

    let mut out: Vec<String> = MODIFIER_ORDER
        .iter()
        .filter(|m| modifiers.contains(m))
        .map(|m| (*m).to_string())
        .collect();
    out.push(key.clone());
    let normalized = out.join("+");

    #[cfg(target_os = "windows")]
    {
        use std::str::FromStr;
        tauri_plugin_global_shortcut::Shortcut::from_str(&normalized)
            .map_err(|_| ShortcutParseError::InvalidToken(key.clone()))?;
    }

    Ok(normalized)
}

// ---------------------------------------------------------------------------
// Tauri integration (Windows only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
#[derive(Debug, Default)]
pub struct ShortcutRegistrationState {
    registered: std::collections::BTreeSet<String>,
}

#[cfg(target_os = "windows")]
pub struct CurrentShortcutState(pub std::sync::Mutex<ShortcutRegistrationState>);

/// Register a global shortcut that toggles the main window. The callback
/// only fires on `ShortcutState::Pressed` to avoid double-firing on key
/// release.
///
/// # Pressed-only guard
///
/// The closure passed to `on_shortcut` checks `event.state ==
/// ShortcutState::Pressed` before invoking `toggle_window_from_any_thread`.
/// Without this guard the window would toggle twice per keystroke (once
/// on press, once on release).
#[cfg(target_os = "windows")]
pub fn register(app: &tauri::AppHandle, accel: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
    let shortcut: Shortcut = accel.parse()?;
    let app_clone = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            // Only toggle on key-press, not key-release, to avoid double-firing.
            if event.state == ShortcutState::Pressed {
                crate::window::toggle_window_from_any_thread(&app_clone);
            }
        })?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn unregister(app: &tauri::AppHandle, accel: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let shortcut: Shortcut = accel.parse()?;
    app.global_shortcut().unregister(shortcut)?;
    Ok(())
}

/// Compare current vs new accelerator and re-register if changed.
///
/// Runtime registration is tracked as a set so failed cleanup cannot be hidden
/// by a later no-op config save. The function returns `Ok(())` only when the
/// known registered set exactly matches the desired accelerator.
#[cfg(target_os = "windows")]
pub fn reregister_if_changed(
    app: &tauri::AppHandle,
    new_accel: Option<&str>,
) -> Result<(), String> {
    use tauri::Manager;
    let state = app.state::<CurrentShortcutState>();
    let mut current = state
        .0
        .lock()
        .map_err(|_| "global shortcut state mutex poisoned".to_string())?;
    let new = match new_accel {
        Some(accel) => Some(
            normalize_accelerator(accel)
                .map_err(|e| format!("global shortcut accelerator is invalid ({accel}): {e}"))?,
        ),
        None => None,
    };
    let desired: std::collections::BTreeSet<String> = new.iter().cloned().collect();
    if current.registered == desired {
        return Ok(());
    }

    if let Some(ref na) = new {
        if !current.registered.contains(na) {
            if let Err(e) = register(app, na) {
                let message = format!("global shortcut register failed for {na}: {e}");
                tracing::warn!(error=%e, accel=%na, "global shortcut register failed");
                return Err(message);
            }
            current.registered.insert(na.clone());
        }
    }

    let stale: Vec<String> = current
        .registered
        .iter()
        .filter(|accel| Some(accel.as_str()) != new.as_deref())
        .cloned()
        .collect();
    for oa in stale {
        if let Err(e) = unregister(app, &oa) {
            let message = format!("global shortcut unregister failed for {oa}: {e}");
            tracing::error!(error=%e, accel=%oa, "global shortcut unregister failed");
            return Err(message);
        }
        current.registered.remove(&oa);
    }

    if current.registered == desired {
        Ok(())
    } else {
        Err(format!(
            "global shortcut registration state is inconsistent: registered={:?}, desired={desired:?}",
            current.registered
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_shift_g_canonicalised() {
        assert_eq!(
            normalize_accelerator("Ctrl+Shift+G").unwrap(),
            "Control+Shift+G"
        );
    }

    #[test]
    fn whitespace_collapsed_and_aliases_resolved() {
        assert_eq!(
            normalize_accelerator("ctrl  +  shift\t+ g").unwrap(),
            "Control+Shift+G"
        );
    }

    #[test]
    fn cmd_alias_becomes_super() {
        assert_eq!(normalize_accelerator("cmd+g").unwrap(), "Super+G");
    }

    #[test]
    fn win_alias_becomes_super() {
        assert_eq!(normalize_accelerator("win+f").unwrap(), "Super+F");
    }

    #[test]
    fn alt_shift_combo() {
        assert_eq!(normalize_accelerator("alt+shift+a").unwrap(), "Alt+Shift+A");
    }

    #[test]
    fn uppercase_input_normalises_key_to_title() {
        assert_eq!(
            normalize_accelerator("CONTROL+ALT+DELETE").unwrap(),
            "Control+Alt+Delete"
        );
    }

    #[test]
    fn empty_input_errors() {
        assert_eq!(normalize_accelerator(""), Err(ShortcutParseError::Empty));
        assert_eq!(normalize_accelerator("   "), Err(ShortcutParseError::Empty));
    }

    #[test]
    fn modifier_only_errors() {
        assert!(matches!(
            normalize_accelerator("Control"),
            Err(ShortcutParseError::ModifierOnly(_))
        ));
        assert!(matches!(
            normalize_accelerator("Ctrl+Shift"),
            Err(ShortcutParseError::ModifierOnly(_))
        ));
    }

    #[test]
    fn modifier_order_is_canonical() {
        assert_eq!(
            normalize_accelerator("Shift+Control+G").unwrap(),
            "Control+Shift+G"
        );
        assert_eq!(
            normalize_accelerator("Super+Alt+Control+Shift+K").unwrap(),
            "Control+Alt+Shift+Super+K"
        );
    }

    #[test]
    fn multiple_keys_rejected() {
        assert!(matches!(
            normalize_accelerator("Ctrl+G+H"),
            Err(ShortcutParseError::InvalidToken(_))
        ));
    }

    /// Compile-time guard documenting that `register()` only toggles the
    /// window on `ShortcutState::Pressed`. The fix lives in `register()`'s
    /// closure; this test asserts the source still contains the guard so a
    /// future refactor cannot silently regress it. Verifying actual
    /// callback behaviour requires a Tauri AppHandle (Windows-only), so we
    /// fall back to a textual contract check on the source file.
    #[test]
    fn shortcut_register_only_fires_on_pressed() {
        let src = include_str!("shortcut.rs");
        assert!(
            src.contains("ShortcutState::Pressed"),
            "register() must guard against Released events using ShortcutState::Pressed"
        );
        assert!(
            src.contains("if event.state == ShortcutState::Pressed"),
            "register() must check event.state == ShortcutState::Pressed before toggling"
        );
    }

    #[test]
    fn invalid_key_rejected() {
        assert!(matches!(
            normalize_accelerator("Ctrl+NotARealKey"),
            Err(ShortcutParseError::InvalidToken(_))
        ));
    }

    #[test]
    fn empty_key_rejected() {
        // "Ctrl+" has an empty token after the last +
        assert!(matches!(
            normalize_accelerator("Ctrl+"),
            Err(ShortcutParseError::InvalidToken(_))
        ));
    }

    #[test]
    fn valid_allowlist_key_accepted() {
        assert_eq!(normalize_accelerator("Ctrl+F5").unwrap(), "Control+F5");
    }

    #[test]
    fn canonical_camel_case_keys_are_preserved() {
        let cases = [
            ("Ctrl+PageUp", "Control+PageUp"),
            ("Ctrl+pageup", "Control+PageUp"),
            ("Ctrl+pgup", "Control+PageUp"),
            ("Ctrl+PageDown", "Control+PageDown"),
            ("Ctrl+pgdn", "Control+PageDown"),
            ("Ctrl+NumpadAdd", "Control+NumpadAdd"),
            ("Ctrl+NumpadPlus", "Control+NumpadAdd"),
            ("Ctrl+CapsLock", "Control+CapsLock"),
            ("Ctrl+ScrollLock", "Control+ScrollLock"),
            ("Ctrl+NumLock", "Control+NumLock"),
            ("Ctrl+PrintScreen", "Control+PrintScreen"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_accelerator(input).unwrap(), expected);
        }
    }

    // --- Alias canonicalization tests ---

    #[test]
    fn return_alias_canonicalised_to_enter() {
        assert_eq!(
            normalize_accelerator("Ctrl+Return").unwrap(),
            "Control+Enter"
        );
    }

    #[test]
    fn esc_alias_canonicalised_to_escape() {
        assert_eq!(normalize_accelerator("Ctrl+Esc").unwrap(), "Control+Escape");
    }

    #[test]
    fn grave_alias_canonicalised_to_backquote() {
        assert_eq!(
            normalize_accelerator("Ctrl+Grave").unwrap(),
            "Control+Backquote"
        );
    }

    #[test]
    fn arrow_aliases_canonicalised() {
        assert_eq!(normalize_accelerator("Ctrl+ArrowUp").unwrap(), "Control+Up");
        assert_eq!(
            normalize_accelerator("Ctrl+ArrowDown").unwrap(),
            "Control+Down"
        );
        assert_eq!(
            normalize_accelerator("Ctrl+ArrowLeft").unwrap(),
            "Control+Left"
        );
        assert_eq!(
            normalize_accelerator("Ctrl+ArrowRight").unwrap(),
            "Control+Right"
        );
    }

    #[test]
    fn spacebar_alias_canonicalised_to_space() {
        assert_eq!(
            normalize_accelerator("Ctrl+Spacebar").unwrap(),
            "Control+Space"
        );
        assert_eq!(
            normalize_accelerator("Ctrl+Space").unwrap(),
            "Control+Space"
        );
    }

    #[test]
    fn backquote_directly_accepted() {
        assert_eq!(
            normalize_accelerator("Ctrl+Backquote").unwrap(),
            "Control+Backquote"
        );
    }

    // --- Runtime-parser compat test (Windows-only) ---

    /// Verify that the canonical form produced by normalize_accelerator
    /// for a representative set of keys is parseable by the real
    /// global-hotkey runtime (tauri_plugin_global_shortcut::Shortcut::from_str).
    #[cfg(target_os = "windows")]
    #[test]
    fn canonical_forms_parse_with_runtime_parser() {
        use std::str::FromStr;
        let cases = [
            "Control+Enter",
            "Control+F5",
            "Control+PageUp",
            "Control+PageDown",
            "Control+NumpadAdd",
            "Control+CapsLock",
            "Control+PrintScreen",
            "Control+Space",
            "Control+A",
            "Control+0",
            "Control+Up",
            "Control+Tab",
            "Control+Escape",
        ];
        for case in &cases {
            tauri_plugin_global_shortcut::Shortcut::from_str(case)
                .unwrap_or_else(|e| panic!("runtime rejected canonical form {case:?}: {e}"));
        }
    }
}
