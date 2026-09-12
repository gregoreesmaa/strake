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

/// Parse an Electron accelerator (`CommandOrControl+Shift+S`).
/// `CmdOrCtrl` is accepted as an alias; modifier matching is
/// case-insensitive; the final token is the key.
pub fn parse_accelerator(text: &str) -> Result<Accelerator, AcceleratorError> {
    let mut modifiers = Vec::new();
    let mut key: Option<&str> = None;
    for token in text
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        // The final token is the key; everything before it must be a known
        // modifier. Matching stays lenient (case-insensitive, aliases) so
        // platform spellings all parse to the same model.
        let modifier = match token.to_ascii_lowercase().as_str() {
            "commandorcontrol" | "cmdorctrl" => Modifier::CommandOrControl,
            "control" | "ctrl" => Modifier::Control,
            "alt" | "option" => Modifier::Alt,
            "shift" => Modifier::Shift,
            "super" | "meta" | "command" | "cmd" => Modifier::Super,
            _ => {
                key = Some(token);
                continue;
            }
        };
        modifiers.push(modifier);
    }
    match key {
        Some(key) => Ok(Accelerator {
            modifiers,
            key: key.to_string(),
        }),
        None if modifiers.is_empty() => Err(AcceleratorError::Empty),
        None => Err(AcceleratorError::Empty),
    }
}

/// One menu item template (`MenuItemConstructorOptions` subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemTemplate {
    /// Stable id for lookup and tests.
    pub id: Option<String>,
    /// Display label (required without a role).
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
    /// A plain labeled item.
    pub fn labeled(label: &str) -> Self {
        Self {
            id: None,
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

impl std::error::Error for MenuError {}

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
        if item.label.is_none() && item.role.is_none() && item.submenu.is_empty() {
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
                        label: None,
                        role: Some(MenuRole::About),
                        accelerator: None,
                        click: None,
                        enabled: true,
                        visible: true,
                        checked: false,
                        submenu: Vec::new(),
                    },
                    MenuItemTemplate {
                        id: None,
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
            matches!(
                MenuTemplate::build(vec![
                    MenuItemTemplate {
                        id: None,
                        ..MenuItemTemplate::labeled("A")
                    },
                    MenuItemTemplate {
                        id: None,
                        ..MenuItemTemplate::labeled("B")
                    },
                ]),
                Ok(_)
            ),
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
}
