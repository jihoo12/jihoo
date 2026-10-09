//! Loads a program and every module it imports.
//!
//! `import a.b` names the file `a/b.jh`. It is looked up next to the importing
//! file first, then in each search directory in order. Each file is loaded once,
//! however many times it is imported, and imports may form cycles.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::Program;
use crate::{parse_file, Error};

#[derive(Debug, Clone)]
pub struct Module {
    /// Prefix of this module's items in JIR, such as `alloc`; empty for the root.
    pub name: String,
    pub file: u16,
    pub program: Program,
    /// `alias -> module index` for this module's imports.
    pub imports: HashMap<String, usize>,
}

pub struct Loaded {
    /// The root module comes first.
    pub modules: Vec<Module>,
    /// Paths by file number, for error messages.
    pub files: Vec<PathBuf>,
}

/// Loads `root` and its imports from the file system.
pub fn load(root: &Path, search: &[PathBuf]) -> Result<Loaded, (Error, Vec<PathBuf>)> {
    load_with(root, search, &|p| std::fs::read_to_string(p).ok())
}

/// Like [`load`], reading files with `read` (which returns `None` for a missing
/// file), so that tests can use files that only exist in memory.
pub fn load_with(
    root: &Path,
    search: &[PathBuf],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Result<Loaded, (Error, Vec<PathBuf>)> {
    let mut l = Loader { search, read, files: Vec::new(), modules: Vec::new(), by_path: HashMap::new() };
    match l.module(root.to_path_buf(), String::new()) {
        Ok(_) => Ok(Loaded { modules: l.modules, files: l.files }),
        Err(e) => Err((e, l.files)),
    }
}

struct Loader<'a> {
    search: &'a [PathBuf],
    read: &'a dyn Fn(&Path) -> Option<String>,
    files: Vec<PathBuf>,
    modules: Vec<Module>,
    by_path: HashMap<PathBuf, usize>,
}

impl Loader<'_> {
    fn module(&mut self, path: PathBuf, name: String) -> Result<usize, Error> {
        if let Some(&m) = self.by_path.get(&path) {
            return Ok(m);
        }
        let file = self.files.len() as u16;
        self.files.push(path.clone());
        let src = (self.read)(&path).ok_or_else(|| Error::new(Default::default(), "cannot read this file"))?;
        let program = parse_file(&src, file)?;

        let index = self.modules.len();
        self.by_path.insert(path.clone(), index);
        let imports = program.imports.clone();
        self.modules.push(Module { name, file, program, imports: HashMap::new() });

        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for imp in imports {
            let rel: PathBuf = imp.path.iter().collect::<PathBuf>().with_extension("jh");
            let found = std::iter::once(&dir)
                .chain(self.search)
                .map(|d| d.join(&rel))
                .find(|p| (self.read)(p).is_some());
            let Some(found) = found else {
                let msg = format!("cannot find module `{}` (looked for `{}`)", imp.path.join("."), rel.display());
                return Err(Error::new(imp.pos, msg));
            };
            if found == path {
                let msg = format!(
                    "`import {}` finds this file itself (`{}`); rename the file, or the module it should import",
                    imp.path.join("."),
                    path.display()
                );
                return Err(Error::new(imp.pos, msg));
            }
            if self.modules[index].imports.contains_key(&imp.alias) {
                return Err(Error::new(imp.pos, format!("`{}` is imported twice", imp.alias)));
            }
            let target = self.module(found, self.module_name(&imp.path))?;
            self.modules[index].imports.insert(imp.alias, target);
        }
        Ok(index)
    }

    /// `a.b` -> `a.b`, made unique if another file already took the name.
    fn module_name(&self, path: &[String]) -> String {
        let base = path.join(".");
        let mut name = base.clone();
        let mut n = 2;
        while self.modules.iter().any(|m| m.name == name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&Path) -> Option<String> + 'a {
        move |p| files.iter().find(|(n, _)| Path::new(n) == p).map(|(_, s)| s.to_string())
    }

    #[test]
    fn loads_imports_once_and_allows_cycles() {
        let files = [
            ("app/main.jh", "import util\nimport lib.alloc\nfn main() {}"),
            ("app/util.jh", "import lib.alloc as mem\nfn u() {}"),
            ("std/lib/alloc.jh", "import util\nfn a() {}"),
            ("std/util.jh", "fn other() {}"),
        ];
        let l = load_with(Path::new("app/main.jh"), &[PathBuf::from("std")], &fs(&files)).map_err(|e| e.0).unwrap();
        // main -> app/util.jh (next to it) and std/lib/alloc.jh (search path).
        // util -> std/lib/alloc.jh again: the same module, loaded once.
        // alloc -> util: not next to it, so std/util.jh, a different file whose
        // name is already taken, hence `util_2`.
        let names: Vec<&str> = l.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["", "util", "lib.alloc", "util_2"]);
        let root = &l.modules[0];
        assert_eq!(root.imports["alloc"], 2);
        assert_eq!(l.modules[root.imports["util"]].imports["mem"], 2);
        assert_eq!(l.modules[2].imports["util"], 3);
        assert_eq!(l.files[3], Path::new("std/util.jh"));
    }

    #[test]
    fn missing_modules_and_parse_errors_carry_the_file() {
        let files = [("main.jh", "import nope\nfn main() {}")];
        let (e, _) = load_with(Path::new("main.jh"), &[], &fs(&files)).err().unwrap();
        assert!(e.msg.contains("cannot find module `nope`"), "{}", e.msg);
        let files = [("alloc.jh", "import alloc\nfn main() {}")];
        let (e, _) = load_with(Path::new("alloc.jh"), &[], &fs(&files)).err().unwrap();
        assert!(e.msg.contains("finds this file itself"), "{}", e.msg);
        let files = [("main.jh", "import bad\nfn main() {}"), ("bad.jh", "fn x( {}")];
        let (e, paths) = load_with(Path::new("main.jh"), &[], &fs(&files)).err().unwrap();
        assert_eq!(paths[e.pos.file as usize], Path::new("bad.jh"));
    }
}
