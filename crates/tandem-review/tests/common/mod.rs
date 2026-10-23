#![allow(dead_code)]
use tandem_core::{DiffSet, FileDiff};
use tandem_review::{TaskMeta, diff_texts};

pub fn set(task: &str, agent: &str, files: Vec<FileDiff>) -> DiffSet {
    DiffSet { task: task.into(), agent: agent.into(), base_commit: "base".into(), files }
}

pub fn meta(task: &str, agent: &str, prompt: &str) -> TaskMeta {
    TaskMeta::new(task, agent, prompt)
}

pub fn modify(path: &str, old: &str, new: &str) -> FileDiff {
    diff_texts(path, Some(old), Some(new))
}

/// Replace 1-based line `n` of `text` with `with` (may contain several lines).
pub fn replace_line(text: &str, n: usize, with: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines[n - 1] = with.to_string();
    lines.join("\n") + "\n"
}

pub fn insert_after(text: &str, n: usize, with: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines.insert(n, with.to_string());
    lines.join("\n") + "\n"
}

pub fn delete_lines(text: &str, from: usize, to: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = lines[..from - 1].to_vec();
    out.extend_from_slice(&lines[to..]);
    out.join("\n") + "\n"
}

pub const RS: &str = r#"use std::fmt;

pub const MAX_RETRIES: u32 = 3;

pub struct Client {
    url: String,
}

impl Client {
    pub fn new(url: &str) -> Self {
        Self { url: url.to_string() }
    }

    pub fn fetch(&self, path: &str) -> String {
        let full = format!("{}/{}", self.url, path);
        let mut out = String::new();
        out.push_str(&full);
        out.push_str("?retry=");
        out.push_str(&MAX_RETRIES.to_string());
        out.push_str("&mode=fast");
        out.push_str("&v=1");
        out
    }
}

pub fn parse_config(input: &str) -> Vec<String> {
    input.lines().map(str::to_string).collect()
}

pub fn helper() -> u32 {
    42
}
"#;

pub const TS: &str = r#"import { api } from "./api";

export const DEFAULT_LIMIT = 50;

export interface User {
  id: string;
  name: string;
}

export class UserService {
  constructor(private base: string) {}

  async load(id: string): Promise<User> {
    const res = await api.get(`${this.base}/users/${id}`);
    return res.json();
  }

  save(user: User): void {
    api.post(`${this.base}/users`, user);
  }
}

export const formatUser = (u: User): string => {
  return `${u.name} <${u.id}>`;
};

function internal() {
  return DEFAULT_LIMIT;
}
"#;

pub const PY: &str = r#"import os

TIMEOUT = 30


class Repo:
    def __init__(self, path):
        self.path = path

    def files(self):
        out = []
        for name in os.listdir(self.path):
            out.append(name)
        return out


def load_settings(path):
    with open(path) as fh:
        return fh.read()


def unrelated():
    return TIMEOUT
"#;

pub const GO: &str = r#"package main

import "fmt"

const Version = "1.0"

type Server struct {
	Port int
}

func (s *Server) Start() error {
	fmt.Println("start", s.Port)
	return nil
}

func Greet(name string) string {
	return fmt.Sprintf("hi %s", name)
}
"#;
