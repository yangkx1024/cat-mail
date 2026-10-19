//! The sidebar's folder hierarchy, built from the flat list the engine stores.

use catmail_core::{Folder, FolderRole};

/// One level of a mailbox name. `folder` is `None` for a parent the server never listed
/// (servers may omit them), which is shown but holds no messages, like `\Noselect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNode {
    pub label: String,
    pub folder: Option<Folder>,
    pub children: Vec<FolderNode>,
}

impl FolderNode {
    /// The folder whose messages this node shows, if it can hold any.
    pub fn selectable(&self) -> Option<&Folder> {
        self.folder.as_ref().filter(|f| f.selectable)
    }
}

/// Nests the folders by their hierarchy delimiter, keeping the engine's order (INBOX
/// first) among siblings.
pub fn folder_tree(folders: &[Folder]) -> Vec<FolderNode> {
    let mut roots = Vec::new();
    for folder in folders {
        let segments: Vec<&str> = match folder.delimiter {
            Some(delimiter) => folder.name.split(delimiter).collect(),
            None => vec![&folder.name],
        };
        let (leaf, parents) = segments.split_last().expect("split yields one segment");
        let mut level = &mut roots;
        for parent in parents {
            let ix = match level.iter().position(|n: &FolderNode| n.label == *parent) {
                Some(ix) => ix,
                None => {
                    level.push(FolderNode {
                        label: (*parent).to_owned(),
                        folder: None,
                        children: Vec::new(),
                    });
                    level.len() - 1
                }
            };
            level = &mut level[ix].children;
        }
        // A parent created as a placeholder for an earlier child takes the real folder.
        match level
            .iter_mut()
            .find(|n| n.label == *leaf && n.folder.is_none())
        {
            Some(node) => node.folder = Some(folder.clone()),
            None => level.push(FolderNode {
                label: (*leaf).to_owned(),
                folder: Some(folder.clone()),
                children: Vec::new(),
            }),
        }
    }
    for node in &mut roots {
        if node
            .folder
            .as_ref()
            .is_some_and(|f| f.role == Some(FolderRole::Inbox))
        {
            // The server spells it INBOX in any case; it is the one name we translate.
            node.label = "Inbox".into();
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, FolderId};

    use super::*;

    fn folder(id: i64, name: &str, delimiter: Option<char>) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(1),
            name: name.into(),
            delimiter,
            role: (name == "INBOX").then_some(FolderRole::Inbox),
            selectable: true,
            unread_count: 0,
            total_count: 0,
            last_synced_at: None,
        }
    }

    fn shape(nodes: &[FolderNode]) -> Vec<(String, Option<i64>, usize)> {
        nodes
            .iter()
            .map(|n| {
                (
                    n.label.clone(),
                    n.folder.as_ref().map(|f| f.id.0),
                    n.children.len(),
                )
            })
            .collect()
    }

    #[test]
    fn nests_by_delimiter_in_engine_order() {
        let tree = folder_tree(&[
            folder(1, "INBOX", Some('/')),
            folder(2, "INBOX/项目", Some('/')),
            folder(3, "Archive", Some('/')),
            folder(4, "Archive/2026", Some('/')),
            folder(5, "已发送", Some('/')),
        ]);
        assert_eq!(
            shape(&tree),
            [
                ("Inbox".into(), Some(1), 1),
                ("Archive".into(), Some(3), 1),
                ("已发送".into(), Some(5), 0),
            ]
        );
        assert_eq!(shape(&tree[0].children), [("项目".into(), Some(2), 0)]);
        assert_eq!(shape(&tree[1].children), [("2026".into(), Some(4), 0)]);
    }

    #[test]
    fn unlisted_parents_become_placeholders() {
        let tree = folder_tree(&[
            folder(1, "Work.2026.Q3", Some('.')),
            folder(2, "Work", Some('.')),
        ]);
        assert_eq!(shape(&tree), [("Work".into(), Some(2), 1)]);
        assert_eq!(shape(&tree[0].children), [("2026".into(), None, 1)]);
        assert!(tree[0].children[0].selectable().is_none());
        assert_eq!(
            shape(&tree[0].children[0].children),
            [("Q3".into(), Some(1), 0)]
        );
    }

    #[test]
    fn without_a_delimiter_the_name_is_one_level() {
        let tree = folder_tree(&[folder(1, "a/b", None)]);
        assert_eq!(shape(&tree), [("a/b".into(), Some(1), 0)]);
    }

    #[test]
    fn noselect_folders_are_not_selectable() {
        let mut parent = folder(1, "Lists", Some('/'));
        parent.selectable = false;
        let tree = folder_tree(&[parent, folder(2, "Lists/rust", Some('/'))]);
        assert!(tree[0].selectable().is_none());
        assert!(tree[0].children[0].selectable().is_some());
    }
}
