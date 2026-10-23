#![allow(dead_code)]
use std::fs;
use std::path::Path;
use tempfile::TempDir;

pub fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// A small polyglot project with a .gitignore, node_modules, dist, target and .git.
pub fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(r, ".gitignore", "dist/\n*.log\nnode_modules/\n");
    write(r, "package.json", "{ \"name\": \"demo\" }\n");
    write(r, "README.md", "# Demo\nTODO: write docs\n");
    write(r, "apple.txt", "apple\n");
    write(r, "Banana.txt", "banana\n");
    write(r, "Zebra.txt", "zebra\n");
    write(r, "debug.log", "TODO in log\n");
    write(r, "dist/bundle.js", "function TODO_bundle() {}\n");
    write(r, "node_modules/pkg/index.js", "function fromNodeModules() { /* TODO */ }\n");
    write(r, "target/debug/x.rs", "fn built() {}\n");
    write(r, ".git/config", "[core]\n");
    write(
        r,
        "src/index.ts",
        "import { FooBarController } from './components/FooBarController';\n// TODO(alice): wire up\nexport const app = new FooBarController();\n",
    );
    write(
        r,
        "src/components/FooBarController.ts",
        r#"export interface Options { debug: boolean }
export type Handler = (o: Options) => void;
export enum Mode { A, B }
export class FooBarController {
  private x = 1;
  handle = () => {};
  constructor() {}
  start(): void {}
  static create() { return new FooBarController(); }
}
export function helper(a: number) { const local = 2; return a + local; }
export const MAX = 10;
export const arrow = (x: number) => x;
"#,
    );
    write(
        r,
        "src/components/Button.tsx",
        "export function Button(props: { label: string }) { return <button>{props.label}</button>; }\nexport default class Panel {}\n",
    );
    write(
        r,
        "src/util.js",
        "class Widget { render() {} }\nfunction baz() {}\nconst qux = () => 1;\nlet counter = 0;\n// todo lowercase\n",
    );
    write(
        r,
        "app/service.py",
        "class Service:\n    def run(self):\n        pass\n\n    @staticmethod\n    def make():\n        pass\n\ndef main():\n    pass\n\nMAX_RETRIES = 3\nname = 'x'  # TODO: rename\n",
    );
    write(
        r,
        "crates/core/src/lib.rs",
        r#"pub struct Foo;
impl Foo { pub fn new() -> Self { Foo } }
pub trait Shape { fn area(&self) -> f64; }
pub enum Color { Red }
pub mod m { pub fn inner() {} }
pub const LIMIT: u32 = 3;
pub type Alias = u32;
pub fn free() {}
macro_rules! mac { () => {}; }
"#,
    );
    write(
        r,
        "cmd/main.go",
        "package main\n\ntype Server struct{}\ntype Handler interface{ Serve() }\nfunc (s *Server) Start() {}\nfunc main() { var local = 1; _ = local }\nconst Version = \"1\"\nvar Global = 2\n",
    );
    dir
}
