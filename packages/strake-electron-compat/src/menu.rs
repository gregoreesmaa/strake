//! Native menu template model (issue #12, Phase-0).
//!
//! Application/tray/context menus (`Menu.buildFromTemplate`, #12's first
//! milestone bullet) need a validated data model before any OS menu exists:
//! roles with platform behavior, parsed accelerators, and template
//! validation. The OS binding (muda/global-hotkey, real menus and tray
//! icons) consumes these templates next; until then the model pins the
//! contract headless.

use std::collections::HashSet;

/// Standard menu roles (Electron's `role` values with platform behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuRole {
    /// About dialog entry.
    About,
    /// Hide / hide-others / unhide / quit (app menu).
    Hide,
    /// Quit the application.
    Quit,
    /// Undo / redo.
    Undo,
    /// Cut / copy / paste / select-all / delete.
    Cut,
    /// macOS services submenu.
    Services,
    /// Window minimize / zoom / front.
    Minimize,
}

/// Keyboard modifiers in an accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// `CommandOrControl` (Command on macOS, Control elsewhere).
    CommandOrControl,
    /// `Control`.
    Control,
    /// `Alt` / `Option`.
    Alt,
    /// `Shift`.
    Shift,
    /// `Super` / Windows / Command (explicit).
    Super,
}

/// A parsed accelerator (`CommandOrControl+Shift+S`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accelerator {
    /// Modifiers in written order.
    pub modifiers: Vec<Modifier>,
    /// The key (single character or named key like `F5`, case-preserved).
    pub key: String,
}

/// Accelerator parse failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceleratorError {
    /// Empty string or only modifiers.
    Empty,
    /// Unknown modifier token.
    UnknownModifier(String),
}

impl std::fmt::Display for AcceleratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "accelerator is empty"),
            Self::UnknownModifier(token) => write!(f, "unknown accelerator modifier '{token}'"),
        }
    }
}

impl std::error::Error for AcceleratorError {}

/// Map a token to its modifier, if it names one. Matching stays lenient
/// (case-insensitive, aliases) so platform spellings all parse to the same
/// model.
fn modifier_token(token: &str) -> Option<Modifier> {
    match token.to_ascii_lowercase().as_str() {
        "commandorcontrol" | "cmdorctrl" => Some(Modifier::CommandOrControl),
        "control" | "ctrl" => Some(Modifier::Control),
        "alt" | "option" => Some(Modifier::Alt),
        "shift" => Some(Modifier::Shift),
        "super" | "meta" | "command" | "cmd" => Some(Modifier::Super),
        _ => None,
    }
}

/// Parse an Electron accelerator (`CommandOrControl+Shift+S`).
/// `CmdOrCtrl` is accepted as an alias; modifier matching is
/// case-insensitive. Tokens are collected first and only the final token
/// may be the key: an unknown token in any earlier position is a mistyped
/// modifier and fails with [`AcceleratorError::UnknownModifier`] instead of
/// silently degrading to a broader shortcut. Duplicate modifiers are
/// deduplicated (first occurrence wins).
pub fn parse_accelerator(text: &str) -> Result<Accelerator, AcceleratorError> {
    let tokens: Vec<&str> = text
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    let Some((&last, head)) = tokens.split_last() else {
        return Err(AcceleratorError::Empty);
    };
    let mut modifiers = Vec::new();
    for token in head {
        match modifier_token(token) {
            Some(modifier) => {
                if !modifiers.contains(&modifier) {
                    modifiers.push(modifier);
                }
            }
            None => return Err(AcceleratorError::UnknownModifier(token.to_string())),
        }
    }
    // A trailing modifier names no key ("Ctrl", "Ctrl+Shift"): still empty.
    if modifier_token(last).is_some() {
        return Err(AcceleratorError::Empty);
    }
    Ok(Accelerator {
        modifiers,
        key: last.to_string(),
    })
}

/// Menu item type (Electron's `type` field in
/// `MenuItemConstructorOptions`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MenuItemType {
    /// A normal clickable item.
    #[default]
    Normal,
    /// A visual divider: needs no label, role, or submenu.
    Separator,
    /// An item opening a submenu.
    Submenu,
    /// A toggleable checkbox item.
    Checkbox,
    /// A mutually exclusive radio item.
    Radio,
}

/// One menu item template (`MenuItemConstructorOptions` subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemTemplate {
    /// Stable id for lookup and tests.
    pub id: Option<String>,
    /// Item type (`type` in Electron; `item_type` here since `type` is a
    /// Rust keyword). Separators are exempt from the label rule.
    pub item_type: MenuItemType,
    /// Display label (required without a role, unless a separator).
    pub label: Option<String>,
    /// Standard behavior role.
    pub role: Option<MenuRole>,
    /// Keyboard shortcut, unparsed (parsed on validation).
    pub accelerator: Option<String>,
    /// Click handler name for the JS binding (handlers live renderer-side).
    pub click: Option<String>,
    /// Greyed out.
    pub enabled: bool,
    /// Hidden.
    pub visible: bool,
    /// Checkbox state.
    pub checked: bool,
    /// Submenu items.
    pub submenu: Vec<MenuItemTemplate>,
}

impl MenuItemTemplate {
    /// A separator item (`{ type: 'separator' }` in Electron): no label,
    /// role, or submenu required.
    pub fn separator() -> Self {
        Self {
            id: None,
            item_type: MenuItemType::Separator,
            label: None,
            role: None,
            accelerator: None,
            click: None,
            enabled: true,
            visible: true,
            checked: false,
            submenu: Vec::new(),
        }
    }

    /// A plain labeled item.
    pub fn labeled(label: &str) -> Self {
        Self {
            id: None,
            item_type: MenuItemType::Normal,
            label: Some(label.to_string()),
            role: None,
            accelerator: None,
            click: None,
            enabled: true,
            visible: true,
            checked: false,
            submenu: Vec::new(),
        }
    }
}

/// Template validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuError {
    /// Neither label, role, nor submenu type marker is present.
    Unlabeled(String),
    /// Two items share an id.
    DuplicateId(String),
    /// The accelerator does not parse.
    BadAccelerator(String, AcceleratorError),
}

impl std::fmt::Display for MenuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unlabeled(where_) => write!(f, "menu item {where_} needs a label or role"),
            Self::DuplicateId(id) => write!(f, "duplicate menu item id '{id}'"),
            Self::BadAccelerator(item, error) => write!(f, "menu item {item}: {error}"),
        }
    }
}

impl std::error::Error for MenuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BadAccelerator(_, error) => Some(error),
            _ => None,
        }
    }
}

/// A validated application/tray menu template (`Menu.buildFromTemplate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuTemplate {
    /// Top-level items.
    pub items: Vec<MenuItemTemplate>,
}

impl MenuTemplate {
    /// Validate ids, labels, and accelerators recursively.
    pub fn build(items: Vec<MenuItemTemplate>) -> Result<Self, MenuError> {
        let mut seen = HashSet::new();
        for (index, item) in items.iter().enumerate() {
            Self::validate_item(item, &format!("at index {index}"), &mut seen)?;
        }
        Ok(Self { items })
    }

    fn validate_item(
        item: &MenuItemTemplate,
        where_: &str,
        seen: &mut HashSet<String>,
    ) -> Result<(), MenuError> {
        if let Some(id) = &item.id
            && !seen.insert(id.clone())
        {
            return Err(MenuError::DuplicateId(id.clone()));
        }
        if item.item_type != MenuItemType::Separator
            && item.label.is_none()
            && item.role.is_none()
            && item.submenu.is_empty()
        {
            return Err(MenuError::Unlabeled(where_.to_string()));
        }
        if let Some(accelerator) = &item.accelerator {
            parse_accelerator(accelerator)
                .map_err(|error| MenuError::BadAccelerator(where_.to_string(), error))?;
        }
        for (index, child) in item.submenu.iter().enumerate() {
            Self::validate_item(child, &format!("{where_} > child {index}"), seen)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_parse_modifiers_aliases_and_keys() {
        let parsed = parse_accelerator("CommandOrControl+Shift+S").expect("parse");
        assert_eq!(
            parsed.modifiers,
            vec![Modifier::CommandOrControl, Modifier::Shift]
        );
        assert_eq!(parsed.key, "S");
        let parsed = parse_accelerator("CmdOrCtrl+Option+F5").expect("alias parse");
        assert_eq!(
            parsed.modifiers,
            vec![Modifier::CommandOrControl, Modifier::Alt]
        );
        assert_eq!(parsed.key, "F5");
        assert_eq!(parse_accelerator(""), Err(AcceleratorError::Empty));
    }

    #[test]
    fn app_menu_template_validates() {
        let template = MenuTemplate::build(vec![
            MenuItemTemplate {
                id: Some(String::from("app")),
                item_type: MenuItemType::Normal,
                label: Some(String::from("Calculator")),
                role: None,
                accelerator: None,
                click: None,
                enabled: true,
                visible: true,
                checked: false,
                submenu: vec![
                    MenuItemTemplate {
                        id: None,
                        item_type: MenuItemType::Normal,
                        label: None,
                        role: Some(MenuRole::About),
                        accelerator: None,
                        click: None,
                        enabled: true,
                        visible: true,
                        checked: false,
                        submenu: Vec::new(),
                    },
                    MenuItemTemplate::separator(),
                    MenuItemTemplate {
                        id: None,
                        item_type: MenuItemType::Normal,
                        label: None,
                        role: Some(MenuRole::Quit),
                        accelerator: Some(String::from("CommandOrControl+Q")),
                        click: None,
                        enabled: true,
                        visible: true,
                        checked: false,
                        submenu: Vec::new(),
                    },
                ],
            },
            MenuItemTemplate {
                id: Some(String::from("edit")),
                item_type: MenuItemType::Normal,
                label: Some(String::from("Edit")),
                role: None,
                accelerator: None,
                click: None,
                enabled: true,
                visible: true,
                checked: false,
                submenu: vec![MenuItemTemplate::labeled("Undo")],
            },
        ])
        .expect("valid template");
        assert_eq!(template.items.len(), 2);
    }

    #[test]
    fn validation_rejects_bad_templates() {
        assert!(
            MenuTemplate::build(vec![
                MenuItemTemplate {
                    id: None,
                    ..MenuItemTemplate::labeled("A")
                },
                MenuItemTemplate {
                    id: None,
                    ..MenuItemTemplate::labeled("B")
                },
            ])
            .is_ok(),
            "id-less items are fine"
        );
        let dup = || MenuItemTemplate {
            id: Some(String::from("x")),
            ..MenuItemTemplate::labeled("X")
        };
        assert_eq!(
            MenuTemplate::build(vec![dup(), dup()]),
            Err(MenuError::DuplicateId(String::from("x")))
        );
        assert!(matches!(
            MenuTemplate::build(vec![MenuItemTemplate {
                label: None,
                role: None,
                ..MenuItemTemplate::labeled("ignored")
            }]),
            Err(MenuError::Unlabeled(_))
        ));
        let mut bad = MenuItemTemplate::labeled("Bad");
        bad.accelerator = Some(String::from(""));
        assert!(matches!(
            MenuTemplate::build(vec![bad]),
            Err(MenuError::BadAccelerator(_, _))
        ));
    }

    #[test]
    fn mistyped_modifiers_error_instead_of_silent_fallback() {
        // "Ctrl+Shft+S" must not degrade to Ctrl+S: the unknown non-final
        // token is a mistyped modifier.
        assert_eq!(
            parse_accelerator("Ctrl+Shft+S"),
            Err(AcceleratorError::UnknownModifier(String::from("Shft")))
        );
        // "Shfit+S" must not degrade to a bare, far broader "S" shortcut.
        assert_eq!(
            parse_accelerator("Shfit+S"),
            Err(AcceleratorError::UnknownModifier(String::from("Shfit")))
        );
        // Leniency survives only for the final key token.
        let parsed = parse_accelerator("Ctrl+Plus").expect("final key stays lenient");
        assert_eq!(parsed.modifiers, vec![Modifier::Control]);
        assert_eq!(parsed.key, "Plus");
        // Trailing modifiers still name no key.
        assert_eq!(parse_accelerator("Ctrl"), Err(AcceleratorError::Empty));
        assert_eq!(
            parse_accelerator("Ctrl+Shift"),
            Err(AcceleratorError::Empty)
        );
    }

    #[test]
    fn duplicate_modifiers_deduplicate() {
        // No existing test pinned this; duplicates normalize (first
        // occurrence wins) so the muda/global-hotkey binding sees one flag.
        let parsed = parse_accelerator("Ctrl+Ctrl+S").expect("dedup parse");
        assert_eq!(parsed.modifiers, vec![Modifier::Control]);
        assert_eq!(parsed.key, "S");
    }

    #[test]
    fn separators_need_no_label_role_or_submenu() {
        // Genuine Electron templates (`{ type: 'separator' }`) validate both
        // top-level and nested, alongside normal items.
        let template = MenuTemplate::build(vec![
            MenuItemTemplate::labeled("File"),
            MenuItemTemplate::separator(),
            MenuItemTemplate {
                id: None,
                ..MenuItemTemplate::labeled("Edit")
            },
        ])
        .expect("separators validate");
        assert_eq!(template.items.len(), 3);
        let nested = MenuTemplate::build(vec![MenuItemTemplate {
            id: None,
            item_type: MenuItemType::Normal,
            label: Some(String::from("View")),
            role: None,
            accelerator: None,
            click: None,
            enabled: true,
            visible: true,
            checked: false,
            submenu: vec![
                MenuItemTemplate::labeled("Reload"),
                MenuItemTemplate::separator(),
                MenuItemTemplate::labeled("Zoom"),
            ],
        }])
        .expect("nested separators validate");
        assert_eq!(nested.items[0].submenu.len(), 3);
        // Non-separator items without label/role/submenu still fail.
        assert!(matches!(
            MenuTemplate::build(vec![MenuItemTemplate {
                item_type: MenuItemType::Normal,
                label: None,
                role: None,
                ..MenuItemTemplate::labeled("ignored")
            }]),
            Err(MenuError::Unlabeled(_))
        ));
    }

    #[test]
    fn bad_accelerator_forwards_source() {
        use std::error::Error;
        let error = MenuError::BadAccelerator(
            String::from("at index 0"),
            AcceleratorError::UnknownModifier(String::from("Shft")),
        );
        let source = error.source().expect("source forwards inner error");
        assert_eq!(source.to_string(), "unknown accelerator modifier 'Shft'");
        assert!(
            MenuError::Unlabeled(String::from("at index 0"))
                .source()
                .is_none(),
            "non-accelerator variants have no source"
        );
        assert!(
            MenuError::DuplicateId(String::from("x")).source().is_none(),
            "non-accelerator variants have no source"
        );
    }
}
