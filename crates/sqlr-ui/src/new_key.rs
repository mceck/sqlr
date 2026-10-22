//! The "New key" dialog of the key browser: a name, a type and the key's first element.

use egui::{Id, TextEdit};
use sqlr_core::KvEdit;

use crate::key_browser::name_edit;
use crate::theme::{self, Spacing};

/// The type of a key the "New key" dialog creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKeyType {
    String,
    Hash,
    List,
    Set,
    ZSet,
    Stream,
}

impl NewKeyType {
    pub const ALL: [NewKeyType; 6] = [
        NewKeyType::String,
        NewKeyType::Hash,
        NewKeyType::List,
        NewKeyType::Set,
        NewKeyType::ZSet,
        NewKeyType::Stream,
    ];

    fn label(self) -> &'static str {
        match self {
            NewKeyType::String => "String",
            NewKeyType::Hash => "Hash",
            NewKeyType::List => "List",
            NewKeyType::Set => "Set",
            NewKeyType::ZSet => "Sorted set",
            NewKeyType::Stream => "Stream",
        }
    }

    /// The labels of the two value fields.
    fn fields(self) -> (&'static str, Option<&'static str>) {
        match self {
            NewKeyType::String => ("Value", None),
            NewKeyType::Hash => ("Field", Some("Value")),
            NewKeyType::List => ("First item", None),
            NewKeyType::Set => ("First member", None),
            NewKeyType::ZSet => ("First member", Some("Score")),
            NewKeyType::Stream => ("Field", Some("Value")),
        }
    }
}

/// What the "New key" dialog has so far.
#[derive(Clone, Debug, PartialEq)]
pub struct NewKeyDraft {
    pub name: String,
    pub ty: NewKeyType,
    pub first: String,
    pub second: String,
    pub error: Option<String>,
}

impl Default for NewKeyDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            ty: NewKeyType::String,
            first: String::new(),
            second: String::new(),
            error: None,
        }
    }
}

impl NewKeyDraft {
    /// The key name and the edit that creates it, or why the draft is not complete.
    pub fn build(&self) -> Result<(Vec<u8>, KvEdit), String> {
        if self.name.is_empty() {
            return Err("Enter a key name".into());
        }
        let first = self.first.as_bytes().to_vec();
        let second = self.second.as_bytes().to_vec();
        let edit = match self.ty {
            NewKeyType::String => KvEdit::SetString(first),
            NewKeyType::Hash => {
                if first.is_empty() {
                    return Err("Enter a field name".into());
                }
                KvEdit::HashSet(first, second)
            }
            NewKeyType::List => KvEdit::ListPush {
                left: false,
                value: first,
            },
            NewKeyType::Set => KvEdit::SetAdd(first),
            NewKeyType::ZSet => {
                let score: f64 = self
                    .second
                    .trim()
                    .parse()
                    .map_err(|_| "The score must be a number".to_string())?;
                KvEdit::ZAdd(first, score)
            }
            NewKeyType::Stream => {
                if first.is_empty() {
                    return Err("Enter a field name".into());
                }
                KvEdit::XAdd(vec![(first, second)])
            }
        };
        // Creating never touches a key that exists.
        Ok((
            self.name.as_bytes().to_vec(),
            KvEdit::Create(Box::new(edit)),
        ))
    }
}

/// The "New key" dialog while `draft` is `Some`. Returns the key and the edit that creates
/// it when the user confirms; cancelling clears the draft.
pub fn new_key_dialog(
    ctx: &egui::Context,
    draft: &mut Option<NewKeyDraft>,
) -> Option<(Vec<u8>, KvEdit)> {
    let d = draft.as_mut()?;
    let mut result = None;
    let mut cancel = false;
    let response = egui::Modal::new(Id::new("kv_new_key")).show(ctx, |ui| {
        ui.set_width(380.0);
        theme::section_title(ui, "New key");
        ui.add_space(Spacing::S);
        theme::field_label(ui, "Key name");
        let name = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut d.name).desired_width(f32::INFINITY),
        );
        name_edit(&name, "Key name");
        theme::field_label(ui, "Type");
        egui::ComboBox::from_id_salt("kv_new_key_type")
            .selected_text(d.ty.label())
            .show_ui(ui, |ui| {
                for ty in NewKeyType::ALL {
                    ui.selectable_value(&mut d.ty, ty, ty.label());
                }
            });
        let (first, second) = d.ty.fields();
        theme::field_label(ui, first);
        let field = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut d.first).desired_width(f32::INFINITY),
        );
        name_edit(&field, first);
        if let Some(second) = second {
            theme::field_label(ui, second);
            let field = crate::text_menu::text_edit(
                ui,
                TextEdit::singleline(&mut d.second).desired_width(f32::INFINITY),
            );
            name_edit(&field, second);
        }
        if let Some(error) = &d.error {
            ui.colored_label(theme::palette(ui.ctx()).error, error);
        }
        ui.add_space(Spacing::L);
        ui.horizontal(|ui| {
            if ui.add(theme::primary_button(ui.ctx(), "Create")).clicked() {
                match d.build() {
                    Ok(done) => result = Some(done),
                    Err(e) => d.error = Some(e),
                }
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if cancel || response.should_close() || result.is_some() {
        *draft = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(edit: KvEdit) -> KvEdit {
        KvEdit::Create(Box::new(edit))
    }

    #[test]
    fn the_new_key_draft_builds_each_type() {
        let mut d = NewKeyDraft {
            name: "k".into(),
            first: "v".into(),
            ..Default::default()
        };
        assert_eq!(
            d.build(),
            Ok((b"k".to_vec(), create(KvEdit::SetString(b"v".to_vec()))))
        );
        d.ty = NewKeyType::Hash;
        d.first = "f".into();
        d.second = "x".into();
        assert_eq!(
            d.build(),
            Ok((
                b"k".to_vec(),
                create(KvEdit::HashSet(b"f".to_vec(), b"x".to_vec()))
            ))
        );
        d.ty = NewKeyType::ZSet;
        d.second = "2.5".into();
        assert_eq!(
            d.build(),
            Ok((b"k".to_vec(), create(KvEdit::ZAdd(b"f".to_vec(), 2.5))))
        );
        d.second = "high".into();
        assert!(d.build().is_err());
        d.ty = NewKeyType::Stream;
        assert_eq!(
            d.build(),
            Ok((
                b"k".to_vec(),
                create(KvEdit::XAdd(vec![(b"f".to_vec(), b"high".to_vec())]))
            ))
        );
        d.name.clear();
        assert!(d.build().is_err());
    }
}
