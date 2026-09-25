use crate::app::SettingsScope;
use scala_core::{
    GpuOffload, ModelArtifact, SettingDefinition, SettingKind, SettingValue,
    UnsignedIntegerOrChoiceValue,
};

#[derive(Debug, Clone)]
pub struct EditorOption {
    pub label: String,
    pub value: Option<SettingValue>,
    pub custom: bool,
}

#[derive(Debug, Clone)]
pub struct SettingsEditor {
    pub definition: SettingDefinition,
    pub scope: SettingsScope,
    pub model: Option<Box<ModelArtifact>>,
    pub metadata: String,
    pub options: Vec<EditorOption>,
    pub selected: usize,
    pub filter: String,
    pub info_page: usize,
    pub items: Vec<String>,
    pub item: usize,
    pub editing_item: bool,
}

impl SettingsEditor {
    pub fn new(
        definition: SettingDefinition,
        scope: SettingsScope,
        model: Option<Box<ModelArtifact>>,
        metadata: String,
        local: Option<&SettingValue>,
    ) -> Self {
        let parent = match &scope {
            SettingsScope::Server => "server baseline",
            SettingsScope::Runtime(_) => "runtime",
            SettingsScope::ModelProfile(_) => "Settings",
        };
        let mut options = vec![EditorOption {
            label: format!("Inherit from {parent}"),
            value: None,
            custom: false,
        }];
        let mut add = |label: String, value: SettingValue| {
            options.push(EditorOption {
                label,
                value: Some(value),
                custom: false,
            })
        };
        match &definition.kind {
            SettingKind::Toggle => {
                add("Enabled".into(), SettingValue::Toggle(true));
                add("Disabled".into(), SettingValue::Toggle(false));
            }
            SettingKind::OneWayFlag => {
                add("Explicitly enable flag".into(), SettingValue::FlagEnabled)
            }
            SettingKind::Choice { choices } if !choices.is_empty() => {
                for choice in choices {
                    add(choice.clone(), SettingValue::Choice(choice.clone()));
                }
            }
            SettingKind::UnsignedIntegerOrChoice { choices, .. } => {
                for choice in choices {
                    add(
                        choice.clone(),
                        SettingValue::UnsignedIntegerOrChoice(
                            UnsignedIntegerOrChoiceValue::Choice(choice.clone()),
                        ),
                    );
                }
            }
            SettingKind::GpuOffload => {
                add("None".into(), SettingValue::GpuOffload(GpuOffload::None));
                add("Auto".into(), SettingValue::GpuOffload(GpuOffload::Auto));
                add("All".into(), SettingValue::GpuOffload(GpuOffload::All));
            }
            _ => {}
        }
        let custom =
            !matches!(&definition.kind, SettingKind::Choice { choices } if !choices.is_empty());
        // Toggle and one-way controls have no text mode.
        let custom = custom
            && !matches!(
                definition.kind,
                SettingKind::Toggle | SettingKind::OneWayFlag
            );
        if custom {
            let label = match definition.kind {
                SettingKind::GpuOffload => "Exact layer count",
                SettingKind::UnsignedIntegerOrChoice { .. } => "Custom number",
                SettingKind::StringList => "Item list",
                SettingKind::JsonObject => "JSON object",
                SettingKind::Path => "Path",
                SettingKind::Choice { .. } => "Text value (open-ended choice)",
                SettingKind::String => "Text value",
                _ => "Number",
            };
            options.push(EditorOption {
                label: label.into(),
                value: None,
                custom: true,
            });
        }
        // Preserve a stale finite value as a draft; validation still rejects it.
        if !custom
            && let Some(value) = local
            && !options.iter().any(|o| o.value.as_ref() == Some(value))
        {
            options.push(EditorOption {
                label: format!("Current local (not advertised): {value}"),
                value: Some(value.clone()),
                custom: false,
            });
        }
        let selected = local.map_or(0, |value| {
            options
                .iter()
                .position(|o| o.value.as_ref() == Some(value))
                .unwrap_or(options.len() - 1)
        });
        let items = match local {
            Some(SettingValue::StringList(items)) => items.clone(),
            _ => vec![],
        };
        Self {
            definition,
            scope,
            model,
            metadata,
            options,
            selected,
            filter: String::new(),
            info_page: 0,
            items,
            item: 0,
            editing_item: false,
        }
    }
    /// Editor for the login-startup row: Enabled/Disabled only. There is no
    /// Inherit option because the row is a live OS registration, not a
    /// layered settings value.
    pub fn startup(definition: SettingDefinition, metadata: String, enabled: bool) -> Self {
        Self {
            definition,
            scope: SettingsScope::Server,
            model: None,
            metadata,
            options: vec![
                EditorOption {
                    label: "Enabled".into(),
                    value: Some(SettingValue::Toggle(true)),
                    custom: false,
                },
                EditorOption {
                    label: "Disabled".into(),
                    value: Some(SettingValue::Toggle(false)),
                    custom: false,
                },
            ],
            selected: usize::from(!enabled),
            filter: String::new(),
            info_page: 0,
            items: Vec::new(),
            item: 0,
            editing_item: false,
        }
    }
    pub fn custom(&self) -> bool {
        self.options[self.selected].custom
    }
    pub fn visible(&self) -> Vec<usize> {
        self.options
            .iter()
            .enumerate()
            .filter(|(i, o)| {
                *i == 0 || o.label.to_lowercase().contains(&self.filter.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect()
    }
    pub fn move_selection(&mut self, delta: isize) {
        let visible = self.visible();
        let index = visible
            .iter()
            .position(|i| *i == self.selected)
            .unwrap_or(0);
        self.selected = visible
            [(index as isize + delta).clamp(0, visible.len().saturating_sub(1) as isize) as usize];
    }
    fn error(error: scala_core::SettingsError) -> String {
        match error {
            scala_core::SettingsError::InvalidValue { reason, .. } => reason,
            other => other.to_string(),
        }
    }
    pub fn value(&self, text: &str) -> Result<Option<SettingValue>, String> {
        let option = &self.options[self.selected];
        let value = if option.custom {
            Some(if self.definition.kind == SettingKind::StringList {
                SettingValue::StringList(self.items.clone())
            } else {
                let value = self.definition.parse(text).map_err(Self::error)?;
                if matches!(
                    value,
                    SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(_))
                        | SettingValue::GpuOffload(
                            GpuOffload::None | GpuOffload::Auto | GpuOffload::All
                        )
                ) {
                    return Err("Enter a non-negative integer, or select a policy above.".into());
                }
                value
            })
        } else {
            option.value.clone()
        };
        if let Some(value) = &value {
            self.definition.validate_value(value).map_err(|e| {
                format!(
                    "{}. To remove this override, select Inherit.",
                    Self::error(e)
                )
            })?;
        }
        Ok(value)
    }
}
