//! The text of the message, formatted: nsh's `RichEditor` over the draft's HTML (a plain
//! draft, a reply's quote, opens as paragraphs and quotes), its images shown from their
//! attachments' files. A save takes the HTML back with `cid:` URLs, and the plain text
//! sent along with it.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use catmail_frontend::composer::rich_text::{Images, doc_to_text, text_to_html};
use nsh_base::rich_editor::commands::CommandRegistry;
use nsh_base::rich_editor::extensions::extension::ExtensionManager;
use nsh_base::rich_editor::extensions::kit::starter_kit;
use nsh_base::rich_editor::formats::{DomParser, ParseOptions, doc_to_html};
use nsh_base::rich_editor::model::{Schema, Whitespace};
use nsh_base::rich_editor::state::{EditorState, EditorStateConfig};
use nsh_base::rich_editor::view::{EditorSession, NodeViews};
use serde_json::{Value, json};

/// The editor of a composer's text. Its commands tell the view's handlers of the change
/// as they run, so it is shared on its own (`&self`), not borrowed from the window's
/// state while they do.
pub struct Rich {
    session: EditorSession,
    schema: Schema,
    commands: Arc<CommandRegistry>,
    images: RefCell<Images>,
    /// The user changed the text. Until then the draft's own is what it says: opening a
    /// plain draft and closing it again doesn't save it as HTML.
    touched: Cell<bool>,
    /// What [`Self::written`] last wrote, and of which revision of the editor's state:
    /// saves of the other fields don't write the text out again.
    written: RefCell<Option<(u64, String, String)>>,
}

impl Rich {
    /// Shows `html`, or else `text`; `images` are the files of the images in the HTML.
    pub fn new(html: Option<&str>, text: &str, images: Images) -> Self {
        // Without what mail can't show: task lists' checkboxes.
        let kit = starter_kit().configure(json!({ "taskList": false, "taskItem": false }));
        let manager = ExtensionManager::with_core(vec![kit]).expect("the starter kit is valid");
        let schema = manager.schema().clone();
        let html = match html {
            Some(html) => html.to_owned(),
            None => text_to_html(text),
        };
        // As written: indentation and runs of spaces (a quoted table, code) are the
        // author's. The HTML is the editor's, or `text_to_html`'s: no whitespace between
        // its tags to keep out.
        let mut options = ParseOptions::default();
        options.preserve_whitespace = Some(Whitespace::Full);
        let mut doc = DomParser::from_schema(&schema).parse(&html, &options);
        if !images.is_empty() {
            let mut json = doc.to_json();
            images.to_files(&mut json);
            doc = schema
                .node_from_json(&json)
                .expect("the schema's document, with other sources");
        }
        let config = EditorStateConfig::new(schema.clone())
            .doc(doc)
            .plugins(manager.plugins());
        let state = EditorState::create(config).expect("a document of the schema");
        Rich {
            session: EditorSession::new(state, NodeViews::new()),
            schema,
            commands: manager.commands().clone(),
            images: RefCell::new(images),
            touched: Cell::new(false),
            written: RefCell::new(None),
        }
    }

    pub fn session(&self) -> EditorSession {
        self.session.clone()
    }

    pub fn touch(&self) {
        self.touched.set(true);
    }

    /// What the text says now, as plain text and as HTML; `None` until the user changes
    /// it.
    pub fn written(&self) -> Option<(String, String)> {
        if !self.touched.get() {
            return None;
        }
        let state = self.session.state();
        if let Some((revision, text, html)) = &*self.written.borrow()
            && *revision == state.revision()
        {
            return Some((text.clone(), html.clone()));
        }
        let mut json = state.doc().to_json();
        let images = self.images.borrow();
        let html = if images.is_empty() {
            doc_to_html(state.doc())
        } else {
            images.to_content_ids(&mut json);
            let doc = self
                .schema
                .node_from_json(&json)
                .expect("the schema's document, with other sources");
            doc_to_html(&doc)
        };
        let text = doc_to_text(&json);
        *self.written.borrow_mut() = Some((state.revision(), text.clone(), html.clone()));
        Some((text, html))
    }

    /// Runs the editor's command `name`; false if it doesn't apply here.
    pub fn run(&self, name: &str, args: Value) -> bool {
        let Ok(command) = self.commands.make(name, &args, &self.schema) else {
            return false;
        };
        let ran = self.session.run(&command);
        self.session.focus();
        ran
    }

    /// The selection is in a node, or has a mark, of type `name`.
    pub fn is_active(&self, name: &str) -> bool {
        self.session.is_active(Some(name), None)
    }

    /// Puts an image of the draft at the selection: its Content-ID, and the file it shows
    /// from.
    pub fn insert_image(&self, content_id: String, path: std::path::PathBuf, name: &str) {
        let src = path.to_string_lossy().into_owned();
        self.images.borrow_mut().insert(content_id, path);
        self.run("setImage", json!({ "src": src, "alt": name }));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn image_src(rich: &Rich) -> String {
        let mut src = String::new();
        rich.session
            .state()
            .doc()
            .descendants(&mut |node, _, _, _| {
                if node.ty().name() == "image" {
                    src = node.attrs()["src"].as_str().unwrap_or_default().to_owned();
                }
                true
            });
        src
    }

    #[test]
    fn a_plain_draft_is_kept_until_changed_then_saved_with_html() {
        let rich = Rich::new(
            None,
            "Thanks\n\nOn Monday, A wrote:\n> hi\n",
            Images::default(),
        );
        assert_eq!(rich.written(), None);
        rich.touch();
        let (text, html) = rich.written().unwrap();
        assert_eq!(text, "Thanks\n\nOn Monday, A wrote:\n> hi\n");
        assert!(
            html.contains("<blockquote><p>hi</p></blockquote>"),
            "{html}"
        );
    }

    #[test]
    fn whitespace_stays_as_written() {
        let text = "    indented\nA  B\tC\n> |  x  |  y |\n";
        let rich = Rich::new(None, text, Images::default());
        rich.touch();
        let (written, html) = rich.written().unwrap();
        assert_eq!(written, text);
        // And again, from the HTML saved.
        let again = Rich::new(Some(&html), "", Images::default());
        again.touch();
        assert_eq!(again.written().unwrap().0, text);
    }

    #[test]
    fn formats_the_selection() {
        let rich = Rich::new(Some("<p>Hello world</p>"), "", Images::default());
        assert!(rich.run(
            "setTextSelection",
            json!({ "position": { "from": 1, "to": 6 } })
        ));
        assert!(rich.run("toggleBold", Value::Null));
        assert!(rich.is_active("bold"));
        assert!(rich.run("toggleBulletList", Value::Null));
        rich.touch();
        let (text, html) = rich.written().unwrap();
        assert_eq!(text, "* Hello world\n");
        assert!(
            html.contains("<ul><li><p><strong>Hello</strong> world</p></li></ul>"),
            "{html}"
        );
    }

    #[test]
    fn images_show_from_their_files_and_are_saved_by_content_id() {
        let mut images = Images::default();
        images.insert("a@catmail".into(), PathBuf::from("/blobs/a"));
        let rich = Rich::new(Some(r#"<p>See <img src="cid:a@catmail"></p>"#), "", images);
        assert_eq!(image_src(&rich), "/blobs/a");
        rich.insert_image("b@catmail".into(), PathBuf::from("/blobs/b"), "b.png");
        // The view's change touches it.
        rich.touch();
        let (text, html) = rich.written().unwrap();
        assert_eq!(text, "See \n");
        assert!(html.contains(r#"src="cid:a@catmail""#), "{html}");
        assert!(
            html.contains(r#"src="cid:b@catmail" alt="b.png""#),
            "{html}"
        );
        assert!(!html.contains("/blobs/"), "{html}");
    }
}
