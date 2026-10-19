//! Exherbo's repositories and their packages, as the system about to be
//! copied knows them: those it has enabled, from their trees, and those it
//! could add, from its `unavailable` and `unavailable-unofficial` indexes.

use std::collections::BTreeSet;
use std::path::Path;

/// Where paludis finds the repositories it uses, one `<name>.conf` each.
const CONFIGURED: &str = "etc/paludis/repositories";
/// Where the trees of the repositories live, `packages/<category>/<name>`.
const TREES: &str = "var/db/paludis/repositories";
/// Every repository paludis knows of, one `<name>.repository` each: the
/// project's and its developers', then others'.
const INDEXES: [&str; 2] = [
    "var/db/paludis/repositories/unavailable",
    "var/db/paludis/repositories/unavailable-unofficial",
];
/// Those the interactive installer offers, when the system does not have
/// them already.
const OFFERED: [&str; 24] = [
    "CleverCloud",
    "compnerd",
    "desktop",
    "gnome",
    "heirecka",
    "ingmar",
    "java",
    "kde",
    "keruspe",
    "media",
    "net",
    "perl",
    "php",
    "platypus",
    "ruby",
    "rust",
    "scientific",
    "selinux",
    "somasis",
    "tcl",
    "texlive",
    "tombriden",
    "vim",
    "xfce",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub description: String,
    /// The repositories adding this one brings along.
    pub dependencies: Vec<String>,
    /// Its packages, `category/name`.
    pub packages: BTreeSet<String>,
}

/// What a root knows of repositories.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// The repositories it uses, by name.
    pub enabled: Vec<String>,
    /// Every repository it knows of, enabled ones included; those enabled
    /// with the packages of their trees.
    known: Vec<Repository>,
}

impl Index {
    pub fn read(root: &Path) -> Self {
        let enabled = enabled(root);
        let mut known: Vec<Repository> = INDEXES
            .iter()
            .filter_map(|index| std::fs::read_dir(root.join(index)).ok())
            .flatten()
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| parse(&std::fs::read_to_string(entry.path()).ok()?))
            .collect();
        for name in &enabled {
            let tree = tree(&root.join(TREES).join(name));
            match known.iter_mut().find(|r| r.name == *name) {
                // The tree is what the system has; the index, what it had
                // when last synced.
                Some(repository) if !tree.is_empty() => repository.packages = tree,
                Some(_) => {}
                None => known.push(Repository {
                    name: name.clone(),
                    packages: tree,
                    ..Repository::default()
                }),
            }
        }
        known.sort_by_key(|r| r.name.to_lowercase());
        Self { enabled, known }
    }

    /// The repositories the interactive installer offers to add.
    pub fn offered(&self) -> impl Iterator<Item = &Repository> {
        self.known
            .iter()
            .filter(|r| OFFERED.contains(&r.name.as_str()) && !self.enabled.contains(&r.name))
    }

    #[cfg(test)]
    pub fn of(enabled: &[&str], known: Vec<Repository>) -> Self {
        Self {
            enabled: enabled.iter().map(|&name| name.to_owned()).collect(),
            known,
        }
    }

    pub fn knows(&self, name: &str) -> bool {
        self.known.iter().any(|r| r.name == name)
    }

    /// `spec`, a package of the enabled repositories or of those `added`
    /// bring: `htop` becomes `sys-process/htop`, unless several categories
    /// have one. Why not, otherwise. Without any repository known (not a
    /// live system), `spec` as it is.
    pub fn resolve(&self, spec: &str, added: &[String]) -> Result<String, String> {
        if self.known.is_empty() {
            return Ok(spec.to_owned());
        }
        let package = spec;
        let available: BTreeSet<&str> = self
            .with_dependencies(added)
            .iter()
            .filter_map(|name| self.known.iter().find(|r| r.name == *name))
            .flat_map(|r| r.packages.iter().map(String::as_str))
            .collect();
        let found: Vec<&str> = available
            .into_iter()
            .filter(|p| {
                *p == package || (!package.contains('/') && p.rsplit('/').next() == Some(package))
            })
            .collect();
        match found.as_slice() {
            [one] => Ok((*one).to_owned()),
            [] => Err(format!(
                "\"{spec}\" is in none of the enabled or added repositories"
            )),
            several => Err(format!("\"{spec}\" is ambiguous: {}", several.join(", "))),
        }
    }

    /// The enabled repositories, `added`, and those they bring along.
    fn with_dependencies(&self, added: &[String]) -> BTreeSet<String> {
        let mut all: BTreeSet<String> = self.enabled.iter().cloned().collect();
        let mut next: Vec<String> = added.to_vec();
        while let Some(name) = next.pop() {
            if all.insert(name.clone())
                && let Some(repository) = self.known.iter().find(|r| r.name == name)
            {
                next.extend(repository.dependencies.iter().cloned());
            }
        }
        all
    }
}

/// The repositories of packages `root` uses: `arbor` and those added to it.
pub fn enabled(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(CONFIGURED))
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".conf")?
                .to_owned();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            (header(&text, "format").as_deref() == Some("e")).then_some(name)
        })
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// The packages of a repository's tree.
fn tree(location: &Path) -> BTreeSet<String> {
    let dirs = |path: &Path| -> Vec<String> {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    };
    let packages = location.join("packages");
    dirs(&packages)
        .into_iter()
        .flat_map(|category| {
            dirs(&packages.join(&category))
                .into_iter()
                .map(move |name| format!("{category}/{name}"))
        })
        .collect()
}

/// A repository's entry in an index: a header, then its packages, a
/// category per line and its packages indented under it.
fn parse(text: &str) -> Option<Repository> {
    let mut packages = BTreeSet::new();
    let mut category = None;
    for line in text.lines().skip_while(|line| !line.trim().is_empty()) {
        if let Some(name) = line.strip_suffix('/') {
            match name.strip_prefix("    ") {
                Some(package) if !package.starts_with(' ') => {
                    if let Some(category) = category {
                        packages.insert(format!("{category}/{package}"));
                    }
                }
                Some(_) => {}
                None => category = Some(name),
            }
        }
    }
    Some(Repository {
        name: header(text, "repo_name")?,
        description: header(text, "description").unwrap_or_default(),
        dependencies: header(text, "dependencies")
            .unwrap_or_default()
            .split(',')
            .filter_map(|d| d.trim().strip_prefix("repository/"))
            .map(str::to_owned)
            .collect(),
        packages,
    })
}

/// `key = value` among the lines before the first blank one.
fn header(text: &str, key: &str) -> Option<String> {
    text.lines()
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET: &str = "format = unavailable-2\nrepo_format = e\nsync = git+https://gitlab.exherbo.org/exherbo/net.git\ndependencies = repository/arbor, repository/x11\nrepo_name = net\ndescription = Network- and www-related applications and libraries.\n\napp-crypt/\n    krb5/\n        :0 1.22.2-r1 ; MIT implementation of the Kerberos V protocol\nnet-misc/\n    mosh/\n        :0 1.4.0 ; Mobile shell\n    htop/\n        :0 1 ; a namesake\n";

    #[test]
    fn an_index_entry_names_describes_and_lists() {
        let net = parse(NET).unwrap();
        assert_eq!(net.name, "net");
        assert_eq!(
            net.description,
            "Network- and www-related applications and libraries."
        );
        assert_eq!(net.dependencies, ["arbor", "x11"]);
        assert_eq!(
            net.packages.into_iter().collect::<Vec<_>>(),
            ["app-crypt/krb5", "net-misc/htop", "net-misc/mosh"]
        );
        assert_eq!(parse("format = unavailable-2\n"), None);
    }

    /// A root with arbor enabled (a tree with sys-process/htop), and net
    /// and CleverCloud in its indexes.
    fn root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "deli-repositories-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("etc/paludis/repositories/arbor.conf", "format = e\n");
        write(
            "etc/paludis/repositories/installed.conf",
            "format = exndbam\n",
        );
        write(
            "var/db/paludis/repositories/arbor/packages/sys-process/htop/htop-3.exheres-0",
            "",
        );
        let index = "var/db/paludis/repositories/unavailable";
        write(&format!("{index}/net.repository"), NET);
        write(
            &format!("{index}/arbor.repository"),
            &NET.replace("repo_name = net", "repo_name = arbor"),
        );
        write(
            &format!("{index}/games.repository"),
            &NET.replace("repo_name = net", "repo_name = games"),
        );
        write(
            &format!("{index}-unofficial/CleverCloud.repository"),
            "repo_name = CleverCloud\ndependencies = repository/net\n\ndev-util/\n    clever-tools/\n",
        );
        root
    }

    #[test]
    fn a_root_offers_what_it_could_add() {
        let root = root();
        let index = Index::read(&root);
        assert_eq!(index.enabled, ["arbor"]);
        assert!(index.knows("games"));
        // Not games: not offered. Not arbor: there already.
        let offered: Vec<&str> = index.offered().map(|r| r.name.as_str()).collect();
        assert_eq!(offered, ["CleverCloud", "net"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bare_names_resolve_when_one_category_has_them() {
        let root = root();
        let index = Index::read(&root);
        // arbor's tree, not its index entry: only sys-process/htop.
        assert_eq!(index.resolve("htop", &[]), Ok("sys-process/htop".into()));
        assert_eq!(
            index.resolve("sys-process/htop", &[]),
            Ok("sys-process/htop".into())
        );
        assert!(index.resolve("mosh", &[]).unwrap_err().contains("none"));
        // CleverCloud brings net along, whose net-misc/htop makes htop
        // ambiguous.
        let added = ["CleverCloud".to_owned()];
        assert_eq!(index.resolve("mosh", &added), Ok("net-misc/mosh".into()));
        assert_eq!(
            index.resolve("clever-tools", &added),
            Ok("dev-util/clever-tools".into())
        );
        let error = index.resolve("htop", &added).unwrap_err();
        assert!(error.contains("net-misc/htop, sys-process/htop"), "{error}");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            Index::default().resolve("anything", &[]),
            Ok("anything".into())
        );
    }
}
