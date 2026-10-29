//! PROTOTYPE, never merged (#53): does a keelrs tool over `Sandbox` that reuses rig-tools'
//! helpers answer exactly as rig-tools' own tool on `Local` does?
//!
//! Each scenario runs on two copies of one fixture: rig's tools (base + confine) on one, the
//! wrapped tools over `sandbox::local()` on the other, through a backend that counts the
//! calls a remote backend would pay a round trip for. Prints both answers and the diff.
//!
//! Run: `cargo run --manifest-path examples/Cargo.toml -p prototype-rig-tools-reuse`

mod wrapped;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use keelrs::Result;
use keelrs::sandbox::{
    ContentHash, Contents, ExecRequest, Execution, Sandbox, SandboxBackend, SandboxPath, Stat,
};
use rig_core::tool::{Tool as _, ToolContext};
use rig_tools::SearchArgs;
use rig_tools::{
    Edit, EditArgs, List, ListArgs, Read, ReadArgs, Replacement, Search, Seen, Write, WriteArgs,
};

const LIB: &str = "pub fn greet(name: &str) -> String {\n    format!(\u{201c}Hello, {name}\u{201d})\n}\n\npub fn twice(x: i32) -> i32 {\n    x * 2\n}\n\npub fn thrice(x: i32) -> i32 {\n    x * 3\n}\n";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base =
        std::env::temp_dir().join(format!("PROTOTYPE-rig-tools-reuse-{}", std::process::id()));
    let (rig_dir, our_dir) = (base.join("rig"), base.join("ours"));
    fixture(&rig_dir)?;
    fixture(&our_dir)?;
    let rig = RigTools::new(&rig_dir);
    let counts = Counts::default();
    let inner = keelrs::sandbox::local().root(&our_dir).open().await?;
    let sb = Sandbox::new(
        Counting {
            inner: inner.clone(),
            counts: counts.clone(),
        },
        inner.dir().clone(),
    );
    let seen = Seen::default();
    let _tools = wrapped::tools(&seen); // they wrap as keelrs sandboxed tools

    let mut same = 0;
    let mut differ = Vec::new();
    macro_rules! case {
        ($name:expr, $rig:expr, $ours:expr) => {{
            counts.reset();
            let (r, o) = ($rig, $ours);
            let (r, o) = (norm(&r, &rig_dir), norm(&o, &our_dir));
            let calls = counts.take();
            println!("\n=== {}   [backend calls: {}]", $name, calls);
            if r == o {
                same += 1;
                println!("SAME\n{}", indent(&r));
            } else {
                differ.push($name);
                println!(
                    "DIFFER\n--- rig\n{}\n--- wrapped\n{}",
                    indent(&r),
                    indent(&o)
                );
            }
        }};
    }

    // read
    case!(
        "read whole",
        rig.read(ReadArgs::new("src/lib.rs")).await,
        show(wrapped::read(&sb, &seen, ReadArgs::new("src/lib.rs")).await)
    );
    case!(
        "read lines 5..2",
        rig.read(ReadArgs::new("src/lib.rs").lines(5, 2)).await,
        show(wrapped::read(&sb, &seen, ReadArgs::new("src/lib.rs").lines(5, 2)).await)
    );
    case!(
        "read missing",
        rig.read(ReadArgs::new("nope.rs")).await,
        show(wrapped::read(&sb, &seen, ReadArgs::new("nope.rs")).await)
    );
    case!(
        "read outside work dir",
        rig.read(ReadArgs::new("../ours/src/lib.rs")).await,
        show(wrapped::read(&sb, &seen, ReadArgs::new("../rig/src/lib.rs")).await)
    );
    case!(
        "read CRLF",
        rig.read(ReadArgs::new("crlf.txt")).await,
        show(wrapped::read(&sb, &seen, ReadArgs::new("crlf.txt")).await)
    );

    // write
    case!(
        "write create",
        rig.write(WriteArgs::new("new/a.txt", "one\ntwo\n")).await,
        show(wrapped::write(&sb, &seen, WriteArgs::new("new/a.txt", "one\ntwo\n")).await)
    );
    case!(
        "write replace",
        rig.write(WriteArgs::new("new/a.txt", "1\n")).await,
        show(wrapped::write(&sb, &seen, WriteArgs::new("new/a.txt", "1\n")).await)
    );

    // edit
    let e = |old: &str, new: &str| EditArgs::new("src/lib.rs", [Replacement::new(old, new)]);
    case!(
        "edit exact (seen whole: write_if path)",
        rig.edit(e("x * 2", "x + x")).await,
        show(wrapped::edit(&sb, &seen, e("x * 2", "x + x")).await)
    );
    case!(
        "edit fuzzy quotes",
        rig.edit(e("    format!(\"Hello, {name}\")", "    format!(\"Hi, {name}\")"))
            .await,
        show(
            wrapped::edit(
                &sb,
                &seen,
                e("    format!(\"Hello, {name}\")", "    format!(\"Hi, {name}\")")
            )
            .await
        )
    );
    case!(
        "edit ambiguous",
        rig.edit(e("pub fn", "fn")).await,
        show(wrapped::edit(&sb, &seen, e("pub fn", "fn")).await)
    );
    case!(
        "edit not found",
        rig.edit(e("x * 9", "x")).await,
        show(wrapped::edit(&sb, &seen, e("x * 9", "x")).await)
    );
    case!(
        "edit replace_all",
        rig.edit(EditArgs::new(
            "src/lib.rs",
            [Replacement::new("i32", "i64").all()]
        ))
        .await,
        show(
            wrapped::edit(
                &sb,
                &seen,
                EditArgs::new("src/lib.rs", [Replacement::new("i32", "i64").all()])
            )
            .await
        )
    );
    case!(
        "edit CRLF kept",
        rig.edit(EditArgs::new("crlf.txt", [Replacement::new("b", "B")]))
            .await,
        show(
            wrapped::edit(
                &sb,
                &seen,
                EditArgs::new("crlf.txt", [Replacement::new("b", "B")])
            )
            .await
        )
    );

    // stale: the file changes behind the model's back after its read
    rig.read(ReadArgs::new("src/lib.rs")).await;
    show(wrapped::read(&sb, &seen, ReadArgs::new("src/lib.rs")).await);
    append(&rig_dir.join("src/lib.rs"), "// someone else\n")?;
    append(&our_dir.join("src/lib.rs"), "// someone else\n")?;
    case!(
        "edit stale, unrelated change (write_if refuses, falls back)",
        rig.edit(e("x * 3", "x + x + x")).await,
        show(wrapped::edit(&sb, &seen, e("x * 3", "x + x + x")).await)
    );
    replace(&rig_dir.join("src/lib.rs"), "x + x + x", "3 * x")?;
    replace(&our_dir.join("src/lib.rs"), "x + x + x", "3 * x")?;
    case!(
        "edit stale, target text changed",
        rig.edit(e("x + x + x", "x * 3")).await,
        show(wrapped::edit(&sb, &seen, e("x + x + x", "x * 3")).await)
    );
    let raw_rig = std::fs::read_to_string(rig_dir.join("src/lib.rs"))?;
    let raw_ours = std::fs::read_to_string(our_dir.join("src/lib.rs"))?;
    case!("file after all edits", raw_rig, raw_ours);

    // search
    case!(
        "search fn",
        rig.search(SearchArgs::new(r"fn \w+")).await,
        show(wrapped::search(&sb, SearchArgs::new(r"fn \w+")).await)
    );
    case!(
        "search 'secret' (gitignored, hidden, target files hold it)",
        rig.search(SearchArgs::new("secret")).await,
        show(wrapped::search(&sb, SearchArgs::new("secret")).await)
    );
    case!(
        "search hidden",
        rig.search(SearchArgs::new("secret").hidden()).await,
        show(wrapped::search(&sb, SearchArgs::new("secret").hidden()).await)
    );
    case!(
        "search files, empty pattern",
        rig.search(SearchArgs::new("").files()).await,
        show(wrapped::search(&sb, SearchArgs::new("").files()).await)
    );
    case!(
        "search in path + glob",
        rig.search(SearchArgs::new("x").in_path("src").glob("*.rs"))
            .await,
        show(wrapped::search(&sb, SearchArgs::new("x").in_path("src").glob("*.rs")).await)
    );

    // list
    case!(
        "list",
        rig.list(ListArgs::default()).await,
        show(wrapped::list(&sb, ListArgs::default()).await)
    );
    case!(
        "list depth 1",
        rig.list(ListArgs::default().depth(1)).await,
        show(wrapped::list(&sb, ListArgs::default().depth(1)).await)
    );
    case!(
        "list hidden",
        rig.list(ListArgs::default().hidden()).await,
        show(wrapped::list(&sb, ListArgs::default().hidden()).await)
    );

    println!(
        "\n=== SUMMARY: {same} same, {} differ: {differ:#?}",
        differ.len()
    );
    std::fs::remove_dir_all(&base)?;
    Ok(())
}

fn show(out: std::result::Result<String, rig_core::tool::ToolExecutionError>) -> String {
    out.unwrap_or_else(|e| format!("ERROR: {e}"))
}

fn norm(text: &str, dir: &Path) -> String {
    let dir = dir.canonicalize().unwrap();
    text.replace(&*dir.to_string_lossy(), "<dir>")
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fixture(dir: &Path) -> std::io::Result<()> {
    let files: &[(&str, &str)] = &[
        ("src/lib.rs", LIB),
        (
            "src/main.rs",
            "fn main() {\n    println!(\"{}\", x());\n}\n",
        ),
        ("crlf.txt", "a\r\nb\r\nc\r\n"),
        ("README.md", "# fixture\n"),
        (".gitignore", "gen/\n"),
        ("gen/out.txt", "secret generated\n"),
        (".hidden/notes.txt", "secret hidden\n"),
        ("target/debug/x.txt", "secret target\n"),
        ("docs/a/b/deep.md", "deep\n"),
    ];
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, text)?;
    }
    // ignore rules apply inside a git repository
    std::fs::create_dir_all(dir.join(".git"))?;
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\nsecret git\n")
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let now = std::fs::read_to_string(path)?;
    std::fs::write(path, now + text)
}

fn replace(path: &Path, from: &str, to: &str) -> std::io::Result<()> {
    let now = std::fs::read_to_string(path)?;
    std::fs::write(path, now.replace(from, to))
}

/// rig-tools' own tools, confined to `dir`, sharing one `Seen`.
struct RigTools {
    read: Read,
    write: Write,
    edit: Edit,
    search: Search,
    list: List,
}

impl RigTools {
    fn new(dir: &Path) -> Self {
        let base = Some(dir.canonicalize().unwrap());
        let seen = Seen::default();
        Self {
            read: Read {
                base: base.clone(),
                confine: true,
                seen: seen.clone(),
                ..Read::default()
            },
            write: Write {
                base: base.clone(),
                confine: true,
                seen: seen.clone(),
                ..Write::default()
            },
            edit: Edit {
                base: base.clone(),
                confine: true,
                seen,
                ..Edit::default()
            },
            search: Search {
                base: base.clone(),
                confine: true,
                ..Search::default()
            },
            list: List {
                base,
                confine: true,
                ..List::default()
            },
        }
    }
    async fn read(&self, a: ReadArgs) -> String {
        show(self.read.call(&mut ToolContext::new(), a).await)
    }
    async fn write(&self, a: WriteArgs) -> String {
        show(self.write.call(&mut ToolContext::new(), a).await)
    }
    async fn edit(&self, a: EditArgs) -> String {
        show(self.edit.call(&mut ToolContext::new(), a).await)
    }
    async fn search(&self, a: SearchArgs) -> String {
        show(self.search.call(&mut ToolContext::new(), a).await)
    }
    async fn list(&self, a: ListArgs) -> String {
        show(self.list.call(&mut ToolContext::new(), a).await)
    }
}

/// Backend calls by operation: what a remote backend pays one round trip each for.
#[derive(Clone, Default)]
struct Counts(Arc<Mutex<BTreeMap<&'static str, usize>>>);

impl Counts {
    fn hit(&self, op: &'static str) {
        *self.0.lock().unwrap().entry(op).or_default() += 1;
    }
    fn reset(&self) {
        self.0.lock().unwrap().clear();
    }
    fn take(&self) -> String {
        let map = std::mem::take(&mut *self.0.lock().unwrap());
        let total: usize = map.values().sum();
        format!("{total} = {map:?}")
    }
}

/// A backend that counts each call, then runs it on `inner`.
struct Counting {
    inner: Sandbox,
    counts: Counts,
}

#[keelrs::async_trait]
impl SandboxBackend for Counting {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        self.counts.hit("stat");
        self.inner.stat(path.as_str()).await
    }
    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        self.counts.hit("read");
        self.inner.read(path.as_str(), limit).await
    }
    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        self.counts.hit("write");
        self.inner.write(path.as_str(), bytes).await
    }
    async fn write_if(
        &self,
        path: &SandboxPath,
        bytes: Vec<u8>,
        held: ContentHash,
    ) -> Result<Option<Contents>> {
        self.counts.hit("write_if");
        self.inner.write_if(path.as_str(), bytes, held).await
    }
    async fn read_files(&self, paths: &[SandboxPath], limit: u64) -> Vec<Option<Vec<u8>>> {
        self.counts.hit("read_files");
        self.inner
            .read_files(&paths.iter().map(SandboxPath::as_str).collect::<Vec<_>>(), limit)
            .await
            .unwrap_or_default()
    }
    async fn remove(&self, path: &SandboxPath, recursive: bool) -> Result<()> {
        self.counts.hit("remove");
        self.inner.remove(path.as_str(), recursive).await
    }
    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        self.counts.hit("rename");
        self.inner.rename(from.as_str(), to.as_str()).await
    }
    async fn list(&self, root: &SandboxPath, glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        self.counts.hit("list");
        self.inner.list(root.as_str(), glob).await
    }
    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        self.counts.hit("canonical");
        self.inner.canonical(path.as_str()).await
    }
    async fn exec(&self, request: ExecRequest) -> Result<Execution> {
        self.counts.hit("exec");
        self.inner.exec(request).await
    }
}

