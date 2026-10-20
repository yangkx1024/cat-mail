//! The sidebar's folder hierarchy, built from the flat list the engine stores.

use catmail_core::{Folder, FolderRole};

/// The order of the folders with a role, which come first under an account whatever
/// their names and nesting. Drafts is not among them: the sidebar's Drafts lists the
/// drafts kept here, of which the server's Drafts folder holds copies.
const ROLE_ORDER: [FolderRole; 7] = [
    FolderRole::Inbox,
    FolderRole::Sent,
    FolderRole::Archive,
    FolderRole::All,
    FolderRole::Flagged,
    FolderRole::Junk,
    FolderRole::Trash,
];

/// An account's folders as the sidebar shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountFolders {
    /// Folders with a role, in [`ROLE_ORDER`], each with its own subfolders.
    pub roles: Vec<FolderNode>,
    /// The rest, as nested on the server.
    pub others: Vec<FolderNode>,
}

impl AccountFolders {
    pub fn new(folders: &[Folder]) -> Self {
        let mut others = folder_tree(folders);
        let mut roles = Vec::new();
        take_roles(&mut others, &mut roles);
        roles.sort_by_key(|node| ROLE_ORDER.iter().position(|r| Some(*r) == node.role()));
        // The Drafts folder itself is replaced by the drafts kept here; its subfolders
        // (rare) are ordinary folders.
        let mut kept = Vec::new();
        for node in roles {
            if node.role() == Some(FolderRole::Drafts) {
                others.extend(node.children);
            } else {
                kept.push(node);
            }
        }
        let mut labelled = Vec::new();
        for node in &mut kept {
            if let Some(role) = node.role()
                && !labelled.contains(&role)
            {
                labelled.push(role);
                node.label = role_label(role).into();
            }
        }
        Self {
            roles: kept,
            others,
        }
    }

    /// Every node, depth first.
    pub fn nodes(&self) -> impl Iterator<Item = &FolderNode> {
        fn walk<'a>(nodes: &'a [FolderNode], out: &mut Vec<&'a FolderNode>) {
            for node in nodes {
                out.push(node);
                walk(&node.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.roles, &mut out);
        walk(&self.others, &mut out);
        out.into_iter()
    }

    /// Every selectable folder in sidebar order, named for a flat list where the tree's
    /// labels won't do (two may be alike, and subfolders of Drafts sit at the top): the
    /// folder the sidebar names for a role by that name, the rest by their path on the
    /// server (`Clients / Invoices`).
    pub fn paths(&self) -> Vec<(&Folder, String)> {
        let mut roles = Vec::new();
        let named: Vec<_> = self
            .roles
            .iter()
            .filter_map(|node| {
                let folder = node.selectable()?;
                let role = folder.role?;
                (!roles.contains(&role)).then(|| {
                    roles.push(role);
                    folder.id
                })
            })
            .collect();
        self.nodes()
            .filter_map(FolderNode::selectable)
            .map(|folder| {
                let path = match (folder.role, folder.delimiter) {
                    (Some(role), _) if named.contains(&folder.id) => role_label(role).into(),
                    (_, Some(delimiter)) => {
                        folder.name.split(delimiter).collect::<Vec<_>>().join(" / ")
                    }
                    (_, None) => folder.name.clone(),
                };
                (folder, path)
            })
            .collect()
    }

    /// The selectable folder with this role, if the account has one.
    pub fn with_role(&self, role: FolderRole) -> Option<&Folder> {
        self.nodes()
            .filter_map(FolderNode::selectable)
            .find(|f| f.role == Some(role))
    }

    /// The node of the selectable folder.
    pub fn find(&self, id: catmail_core::FolderId) -> Option<&FolderNode> {
        self.nodes()
            .find(|node| node.selectable().is_some_and(|f| f.id == id))
    }
}

/// The names the sidebar gives folders with a role, whatever the server calls them.
fn role_label(role: FolderRole) -> &'static str {
    match role {
        FolderRole::Inbox => "Inbox",
        FolderRole::Sent => "Sent",
        FolderRole::Drafts => "Drafts",
        FolderRole::Trash => "Trash",
        FolderRole::Junk => "Junk",
        FolderRole::Archive => "Archive",
        FolderRole::All => "All Mail",
        FolderRole::Flagged => "Flagged",
    }
}

/// Moves the selectable folders with a role, at any depth, from `nodes` to `roles`,
/// subfolders first, so a Sent inside INBOX (`INBOX.Sent`) is found too. Parents left
/// with nothing to show go.
fn take_roles(nodes: &mut Vec<FolderNode>, roles: &mut Vec<FolderNode>) {
    let mut ix = 0;
    while ix < nodes.len() {
        take_roles(&mut nodes[ix].children, roles);
        let node = &nodes[ix];
        if node.selectable().is_some() && node.role().is_some() {
            roles.push(nodes.remove(ix));
        } else if node.folder.is_none() && node.children.is_empty() {
            nodes.remove(ix);
        } else {
            ix += 1;
        }
    }
}

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

    pub fn role(&self) -> Option<FolderRole> {
        self.folder.as_ref().and_then(|f| f.role)
    }

    /// The unread count worth showing: unread mail in Sent, Junk or Trash is no news.
    pub fn unread(&self) -> u32 {
        match self.role() {
            Some(FolderRole::Sent | FolderRole::Junk | FolderRole::Trash) => 0,
            _ => self.selectable().map_or(0, |f| f.unread_count),
        }
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

    fn with_role(id: i64, name: &str, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(1),
            name: name.into(),
            delimiter: Some('.'),
            role: Some(role),
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
                ("INBOX".into(), Some(1), 1),
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
    fn role_folders_come_first_wherever_they_are() {
        let account = AccountFolders::new(&[
            with_role(1, "INBOX", FolderRole::Inbox),
            with_role(2, "INBOX.Trash", FolderRole::Trash),
            folder(3, "INBOX.Projects", Some('.')),
            with_role(4, "INBOX.已发送", FolderRole::Sent),
            with_role(5, "INBOX.Drafts", FolderRole::Drafts),
            folder(6, "INBOX.Drafts.Templates", Some('.')),
            folder(7, "Lists.rust", Some('.')),
            with_role(8, "Lists.Junk", FolderRole::Junk),
        ]);
        assert_eq!(
            shape(&account.roles),
            [
                ("Inbox".into(), Some(1), 1),
                ("Sent".into(), Some(4), 0),
                ("Junk".into(), Some(8), 0),
                ("Trash".into(), Some(2), 0),
            ]
        );
        assert_eq!(
            shape(&account.roles[0].children),
            [("Projects".into(), Some(3), 0)]
        );
        // Drafts gives way to the local drafts; its subfolder stays.
        assert_eq!(
            shape(&account.others),
            [("Lists".into(), None, 1), ("Templates".into(), Some(6), 0)]
        );
        assert_eq!(account.with_role(FolderRole::Sent).unwrap().id, FolderId(4));
        assert!(account.with_role(FolderRole::Archive).is_none());
        // Everything but Drafts, and the "Lists" placeholder.
        assert_eq!(account.nodes().count(), 8);
    }

    #[test]
    fn noselect_folders_are_not_selectable() {
        let mut parent = folder(1, "Lists", Some('/'));
        parent.selectable = false;
        let tree = folder_tree(&[parent, folder(2, "Lists/rust", Some('/'))]);
        assert!(tree[0].selectable().is_none());
        assert!(tree[0].children[0].selectable().is_some());
    }

    #[test]
    fn paths_tell_folders_of_the_same_name_apart() {
        let mut lists = folder(4, "Lists", Some('.'));
        lists.selectable = false;
        let account = AccountFolders::new(&[
            with_role(1, "INBOX", FolderRole::Inbox),
            folder(2, "Clients.Invoices", Some('.')),
            folder(3, "Personal.Invoices", Some('.')),
            lists,
            folder(5, "Lists.rust", Some('.')),
            with_role(6, "Drafts", FolderRole::Drafts),
            folder(7, "Drafts.Templates", Some('.')),
            folder(8, "Templates", Some('.')),
            with_role(9, "INBOX.Trash", FolderRole::Trash),
            with_role(10, "Old.Trash", FolderRole::Trash),
        ]);
        let paths: Vec<_> = account
            .paths()
            .into_iter()
            .map(|(folder, path)| (folder.id.0, path))
            .collect();
        // Placeholders and `\Noselect` folders aren't destinations, but name the way; so
        // does Drafts, whose subfolder the sidebar shows at the top. A second folder with
        // a role isn't the one the sidebar names for it.
        assert_eq!(
            paths,
            [
                (1, "Inbox".into()),
                (9, "Trash".into()),
                (10, "Old / Trash".into()),
                (2, "Clients / Invoices".into()),
                (3, "Personal / Invoices".into()),
                (5, "Lists / rust".into()),
                (8, "Templates".into()),
                (7, "Drafts / Templates".into()),
            ]
        );
    }
}
