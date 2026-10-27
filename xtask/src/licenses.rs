//! 第三方许可证：`cargo xtask licenses` 生成程序内嵌的依赖清单；打包时用 cargo-about 生成含全文的 HTML。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use serde_json::Value;

use crate::run;

/// 发布的目标平台。依赖按平台取并集。
const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// 程序内嵌的清单，每行 `名称\t版本\t许可证`。
pub(crate) const LIST: &str = "crates/scell-ui/assets/licenses.tsv";

/// 内置字体 SCell Mono 的来源（见 xtask/fonts/build.py），不在 Cargo 依赖里，单独列出。
const FONT: (&str, &str, &str) = ("Maple Mono", "7.9", "OFL-1.1");
/// 字体的许可证全文，由 `cargo xtask fonts` 随字体一起取得。
const FONT_LICENSE: &str = "crates/scell-ui/assets/fonts/OFL.txt";
/// 许可证模板中字体一节的占位。
const FONT_MARKER: &str = "<!-- bundled-fonts -->";

/// 写入（或在 `check` 时只比对）依赖清单。
pub(crate) fn licenses(root: &Path, check: bool) -> anyhow::Result<()> {
    let mut rows = BTreeSet::new();
    let (name, version, license) = FONT;
    rows.insert((name.to_owned(), version.to_owned(), license.to_owned()));
    for target in TARGETS {
        rows.extend(dependencies(root, target)?);
    }
    let text: String = rows
        .into_iter()
        .map(|(name, version, license)| format!("{name}\t{version}\t{license}\n"))
        .collect();
    let path = root.join(LIST);
    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != text {
            bail!("{LIST} 与依赖不一致，运行 cargo xtask licenses 重新生成");
        }
        return Ok(());
    }
    std::fs::write(&path, text)?;
    println!("{}", path.display());
    Ok(())
}

/// scell 在 `target` 上实际链接的依赖（普通与构建依赖，不含开发依赖与工作区成员）。
fn dependencies(root: &Path, target: &str) -> anyhow::Result<Vec<(String, String, String)>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            target,
        ])
        .output()
        .context("无法执行 cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let str_of = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let packages: HashMap<String, &Value> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| (str_of(p, "id"), p))
        .collect();
    let members: HashSet<String> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    let nodes: HashMap<String, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| (str_of(n, "id"), n))
        .collect();
    let start = members
        .iter()
        .find(|id| packages.get(*id).is_some_and(|p| p["name"] == "scell"))
        .context("找不到 scell 包")?;
    let mut seen = HashSet::new();
    let mut stack = vec![start.clone()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for dep in nodes[&id]["deps"].as_array().into_iter().flatten() {
            let linked = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null() || k["kind"] == "build");
            if linked {
                stack.push(str_of(dep, "pkg"));
            }
        }
    }
    Ok(seen
        .into_iter()
        .filter(|id| !members.contains(id))
        .map(|id| {
            let p = packages[&id];
            let license = p["license"].as_str().unwrap_or("NOASSERTION");
            (str_of(p, "name"), str_of(p, "version"), license.to_owned())
        })
        .collect())
}

/// 用 cargo-about 生成含全部许可证全文的 HTML，供打包；内置字体的许可证另外补进去。
pub(crate) fn notices(root: &Path, out: &Path) -> anyhow::Result<PathBuf> {
    let file = out.join("THIRD-PARTY-LICENSES.html");
    let dir = root.join("packaging/licenses");
    run(Command::new("cargo")
        .current_dir(root)
        .args(["about", "generate", "--locked", "--fail"])
        .arg("--config")
        .arg(dir.join("about.toml"))
        .arg("--output-file")
        .arg(&file)
        .arg(dir.join("about.hbs")))
    .context("需要 cargo-about：cargo install --locked cargo-about --features cli")?;

    let html = std::fs::read_to_string(&file)?;
    if !html.contains(FONT_MARKER) {
        bail!("about.hbs 缺少占位 {FONT_MARKER}");
    }
    let text = std::fs::read_to_string(root.join(FONT_LICENSE))
        .with_context(|| format!("缺少 {FONT_LICENSE}，运行 cargo xtask fonts 生成"))?;
    let (name, version, license) = FONT;
    let section = format!(
        "<h3 id=\"bundled-font\">{license}</h3>\n\
         <p>Used by the bundled font SCell Mono, a modified version of {name} {version} \
         (<a href=\"https://github.com/subframe7536/maple-font\">github.com/subframe7536/maple-font</a>).</p>\n\
         <pre>{}</pre>",
        escape_html(&text)
    );
    std::fs::write(&file, html.replace(FONT_MARKER, &section))?;
    Ok(file)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
