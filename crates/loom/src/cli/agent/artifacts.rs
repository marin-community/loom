//! `loom artifacts` — named, versioned documents and the threads discussed on them.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use weaver_api::operations::artifacts;

use super::{branch_key, client, truncate};

#[derive(Subcommand)]
pub enum ArtifactCmd {
    /// Write an artifact: append a new revision (creating it if absent). Reads
    /// `<file>`, or stdin when `<file>` is `-` or omitted.
    ///
    /// An image file (`.png`, `.jpg`, `.gif`, `.webp`, `.svg`, …; raster formats
    /// are also recognised from stdin by their magic bytes) is stored as an
    /// `image` artifact backed by a base64 data URI, so it renders inline in
    /// loom — no need to hand-roll the data URI. A `.html`/`.htm` file is stored
    /// as the `html` kind, which loom renders in a sandboxed iframe.
    Write {
        /// The artifact name (its identity within the scope), e.g. `plan`.
        name: String,
        /// File to read the content from; `-` or omitted reads stdin.
        file: Option<String>,
        /// A human title for the artifact (envelope metadata).
        #[arg(long, default_value = "")]
        title: String,
        /// The content kind: `markdown` (the default; GFM + mermaid) or `html`
        /// (rendered in a sandboxed iframe). A `.html`/`.htm` file picks `html`
        /// on its own. Ignored for image files, which use the `image` kind; any
        /// other value is stored verbatim and shown as source.
        #[arg(long, default_value = "markdown")]
        kind: String,
        /// Publish repo-shared (visible to every branch) instead of scoping it
        /// to the current branch.
        #[arg(long)]
        repo: bool,
        /// Reject the write unless this is still the latest revision (0 guards creation).
        #[arg(long)]
        base_rev: Option<i64>,
    },
    /// List artifacts: this branch's plus the repo-shared ones. `--repo` lists
    /// every artifact in the repo, all scopes.
    #[command(name = "list", visible_alias = "ls")]
    Ls {
        /// List every artifact in the repo, regardless of scope.
        #[arg(long)]
        repo: bool,
    },
    /// Show an artifact's content (latest revision by default). `--meta` prints
    /// the envelope (id, name, kind, title, scope, latest rev, timestamps).
    #[command(name = "get", visible_alias = "show")]
    Show {
        name: String,
        /// Show a specific revision instead of the latest.
        #[arg(long)]
        rev: Option<i64>,
        /// Print the envelope metadata instead of the content.
        #[arg(long)]
        meta: bool,
    },
    /// Remove an artifact and its entire revision history. Resolves the name
    /// branch-scoped first, then repo-shared (what `show` would display); pass
    /// `--repo` to target the repo-shared one when a branch copy shadows it.
    #[command(name = "delete", visible_alias = "rm")]
    Rm {
        /// The artifact name to remove.
        name: String,
        /// Remove the repo-shared artifact of this name, not the branch-scoped
        /// one.
        #[arg(long)]
        repo: bool,
    },
    /// List immutable revision metadata, newest first.
    History {
        name: String,
        /// Read the repo-shared artifact rather than a branch copy.
        #[arg(long)]
        repo: bool,
    },
    /// Comment on an artifact: anchor a new discussion thread to a quoted
    /// span, or reply to an existing one.
    ///
    /// Without `--thread`, `--quote` is required and opens a new thread
    /// anchored to that text (plus optional `--prefix`/`--suffix` context for
    /// disambiguation), with `<body>` as its first comment. With `--thread
    /// <id>`, `<body>` is appended as the next reply. The CLI always comments
    /// as `agent` — the human side of the conversation comes through the API.
    Comment {
        /// The artifact name.
        name: String,
        /// Reply to this existing thread instead of starting a new one.
        #[arg(long)]
        thread: Option<i64>,
        /// The text the new thread anchors to. Required unless `--thread`.
        #[arg(long)]
        quote: Option<String>,
        /// A little context just before the quote, for disambiguation.
        #[arg(long, default_value = "")]
        prefix: String,
        /// A little context just after the quote, for disambiguation.
        #[arg(long, default_value = "")]
        suffix: String,
        /// The comment text. Joined with spaces.
        body: Vec<String>,
    },
    /// Resolve a discussion thread on an artifact.
    Resolve {
        /// The artifact name.
        name: String,
        /// The thread id (see `loom artifacts threads`).
        thread_id: i64,
    },
    /// List an artifact's discussion threads, each with its comments. Open
    /// threads only by default; `--all` also shows resolved/orphaned ones.
    Threads {
        /// The artifact name.
        name: String,
        /// Include resolved and orphaned threads too.
        #[arg(long)]
        all: bool,
    },
}

pub async fn run(cmd: ArtifactCmd) -> Result<()> {
    cmd_artifact(cmd).await
}

/// Read raw bytes from a file path, or stdin when `path` is `None` or `"-"`.
fn read_bytes_or_stdin(path: Option<&str>) -> Result<Vec<u8>> {
    use std::io::Read;
    match path {
        Some(p) if p != "-" => std::fs::read(p).map_err(|e| anyhow!("reading {p}: {e}")),
        _ => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .map_err(|e| anyhow!("reading stdin: {e}"))?;
            Ok(buf)
        }
    }
}

/// The largest image we embed inline. base64 inflates by ~⅓, and the data URI
/// rides in the artifact's content column / JSON views / SSE — a few MB is a
/// generous ceiling for a screenshot or diagram; past it, downscale first.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// The image MIME type for a filename's extension, or `None` if it isn't a
/// recognised image extension. Case-insensitive.
fn image_mime_from_ext(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// True when `filename` looks like a standalone HTML document (`.html`/`.htm`),
/// so a plain `loom artifacts write report report.html` lands as the `html`
/// kind loom renders in a sandboxed iframe — no `--kind html` needed. Only
/// promotes from the default `markdown`; an explicit `--kind` always wins.
fn is_html_ext(filename: Option<&str>) -> bool {
    filename
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
        .unwrap_or(false)
}

/// Sniff a raster image's MIME type from its leading magic bytes — for content
/// read from stdin, where there is no extension to go by. Only the unambiguous
/// binary formats are sniffed; text-ish SVG is recognised by extension alone, so
/// that markdown which merely embeds an `<svg>` is never mistaken for an image.
fn image_mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// If `bytes` is a recognised image (by `filename` extension, else raster magic
/// bytes), encode it as a data URI for an `image` artifact. `None` means "not an
/// image — treat as text".
fn encode_image_data_uri(filename: Option<&str>, bytes: &[u8]) -> Result<Option<String>> {
    use base64::Engine;
    let mime = filename
        .and_then(image_mime_from_ext)
        .or_else(|| image_mime_from_magic(bytes));
    let Some(mime) = mime else { return Ok(None) };
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!(
            "image is {:.1} MB; the inline limit is {} MB — downscale it first",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024)
        );
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Some(format!("data:{mime};base64,{b64}")))
}

/// Read, write, and list artifacts — named, versioned documents stored in
/// loom. Scoped to the current branch by default; `--repo` is repo-shared.
async fn cmd_artifact(cmd: ArtifactCmd) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    match cmd {
        ArtifactCmd::Write {
            name,
            file,
            title,
            kind,
            repo,
            base_rev,
        } => {
            let raw = read_bytes_or_stdin(file.as_deref())?;
            // An image becomes an `image` artifact backed by a base64 data URI;
            // everything else is stored as text under the requested kind. A
            // `.html`/`.htm` file promotes the default `markdown` to `html`
            // (loom sandboxes it in an iframe); an explicit `--kind` always
            // wins.
            let (kind, content): (String, String) =
                match encode_image_data_uri(file.as_deref(), &raw)? {
                    Some(uri) => ("image".to_string(), uri),
                    None => {
                        let text = String::from_utf8(raw).map_err(|_| {
                            anyhow!(
                                "artifact content is not valid UTF-8 — \
                                 only text and image files are supported"
                            )
                        })?;
                        let kind = kind.trim();
                        let kind = if kind == "markdown" && is_html_ext(file.as_deref()) {
                            "html".to_string()
                        } else {
                            kind.to_string()
                        };
                        (kind, text)
                    }
                };
            let view = client
                .invoke::<artifacts::write::Op>(&artifacts::write::Input {
                    name: name.trim().to_string(),
                    content,
                    title: Some(title.trim().to_string()),
                    kind: Some(kind),
                    base_rev,
                    repo,
                    branch: key.to_string(),
                })
                .await?;
            // The write already succeeded — loom is definitionally reachable at
            // this point, so the dashboard link is always known now (unlike the
            // direct-db days, when it depended on `$WEAVER_API`/`loom.json`
            // happening to be present). The server resolves the link so it
            // carries its externally-visible origin (`auth.base_url`, else the
            // request Host) — the loopback/wildcard `$WEAVER_API` we dialed
            // (often `http://0.0.0.0:7878`) is not a URL anyone can open. If that
            // resolution fails, fall back to the dialed base rather than lose the
            // rev line entirely.
            let url = client
                .branch_artifact_url(&key, &view.meta.name)
                .await
                .unwrap_or_else(|_| {
                    format!("{}/s/{key}/artifacts/{}", client.base(), view.meta.name)
                });
            let scope = if repo { "repo-shared" } else { "this branch" };
            println!("{url}  (rev {}, {scope})", view.meta.rev);
        }
        ArtifactCmd::Ls { repo } => {
            let artifacts = client
                .invoke::<artifacts::list::Op>(&artifacts::list::Input {
                    repo,
                    branch: key.to_string(),
                })
                .await?;
            if artifacts.is_empty() {
                println!("(no artifacts)");
                return Ok(());
            }
            for a in &artifacts {
                // A branch-scoped artifact is prefixed by its owning branch id;
                // a repo-shared one is marked so the scope is legible at a glance.
                let scope = match &a.branch_id {
                    Some(bid) => format!("{bid}/"),
                    None => "repo:".to_string(),
                };
                let title = if a.title.is_empty() {
                    String::new()
                } else {
                    format!("  {}", a.title)
                };
                println!("{scope}{:<24} [rev {}] {}{title}", a.name, a.rev, a.kind);
            }
        }
        ArtifactCmd::Show { name, rev, meta } => {
            let view = client
                .invoke::<artifacts::get::Op>(&artifacts::get::Input {
                    name: name.trim().to_string(),
                    rev,
                    repo: false,
                    branch: key.to_string(),
                })
                .await?;
            if meta {
                println!("id:      {}", view.meta.id);
                println!("name:    {}", view.meta.name);
                println!("kind:    {}", view.meta.kind);
                if !view.meta.title.is_empty() {
                    println!("title:   {}", view.meta.title);
                }
                println!(
                    "scope:   {}",
                    match &view.meta.branch_id {
                        Some(bid) => format!("branch {bid}"),
                        None => "repo-shared".to_string(),
                    }
                );
                println!("rev:     {}", view.meta.rev);
                println!("created: {}", view.meta.created_at);
                println!("updated: {}", view.meta.updated_at);
                return Ok(());
            }
            print!("{}", view.content);
        }
        ArtifactCmd::Rm { name, repo } => {
            // Fetch first (branch-scoped resolution, matching `show`) so we can
            // report the scope/revision that got removed.
            let a = client
                .invoke::<artifacts::get::Op>(&artifacts::get::Input {
                    name: name.trim().to_string(),
                    rev: None,
                    repo,
                    branch: key.to_string(),
                })
                .await
                .map_err(|_| {
                    anyhow!("no artifact '{}' — see `loom artifacts list`", name.trim())
                })?;
            client
                .delete_branch_artifact(&key, name.trim(), repo)
                .await?;
            let scope = match &a.meta.branch_id {
                Some(bid) => format!("branch {bid}"),
                None => "repo-shared".to_string(),
            };
            println!("deleted {} ({scope}, was rev {})", a.meta.name, a.meta.rev);
        }
        ArtifactCmd::History { name, repo } => {
            let artifact = client
                .invoke::<artifacts::get::Op>(&artifacts::get::Input {
                    name: name.trim().to_string(),
                    rev: None,
                    repo,
                    branch: key.to_string(),
                })
                .await?;
            for version in artifact.versions {
                println!(
                    "rev {}  {}  {}",
                    version.rev, version.created_at, version.author
                );
            }
        }
        ArtifactCmd::Comment {
            name,
            thread,
            quote,
            prefix,
            suffix,
            body,
        } => {
            let name = name.trim();
            let body = body.join(" ");
            if body.trim().is_empty() {
                bail!("a comment body is required");
            }
            match thread {
                Some(thread_id) => {
                    let c = client
                        .add_branch_thread_comment(&key, name, thread_id, &body)
                        .await?;
                    println!("added comment #{} to thread {thread_id} on {name}", c.seq);
                }
                None => {
                    let quote = quote.ok_or_else(|| {
                        anyhow!(
                            "--quote is required to start a new thread \
                             (or pass --thread <id> to reply)"
                        )
                    })?;
                    let a = client
                        .invoke::<artifacts::get::Op>(&artifacts::get::Input {
                            name: name.to_string(),
                            rev: None,
                            repo: false,
                            branch: key.to_string(),
                        })
                        .await?;
                    let t = client
                        .invoke::<artifacts::threads::comment::Op>(
                            &artifacts::threads::comment::Input {
                                name: name.to_string(),
                                body: body.to_string(),
                                target: artifacts::threads::comment::CommentTarget::New {
                                    base_rev: a.meta.rev,
                                    anchor: (weaver_api::AnchorDto {
                                        quote,
                                        prefix,
                                        suffix,
                                    }),
                                },
                                branch: key.to_string(),
                            },
                        )
                        .await?;
                    println!("opened thread {} on {name} (rev {})", t.id, a.meta.rev);
                }
            }
        }
        ArtifactCmd::Resolve { name, thread_id } => {
            client
                .resolve_branch_thread(&key, name.trim(), thread_id)
                .await?;
            println!("resolved thread {thread_id} on {}", name.trim());
        }
        ArtifactCmd::Threads { name, all } => {
            let name = name.trim();
            let threads = client
                .invoke::<artifacts::threads::list::Op>(&artifacts::threads::list::Input {
                    name: name.to_string(),
                    open_only: false,
                    branch: key.to_string(),
                })
                .await?;
            let threads: Vec<_> = if all {
                threads
            } else {
                threads.into_iter().filter(|t| t.status == "open").collect()
            };
            if threads.is_empty() {
                let scope = if all { "" } else { "open " };
                println!("(no {scope}threads on {name})");
                return Ok(());
            }
            for t in &threads {
                println!(
                    "#{} [{}] \"{}\"",
                    t.id,
                    t.status,
                    truncate(&t.anchor.quote, 70)
                );
                for c in &t.comments {
                    println!("    {}: {}", c.author, c.body);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_map_to_mime_case_insensitively() {
        assert_eq!(image_mime_from_ext("shot.png"), Some("image/png"));
        assert_eq!(image_mime_from_ext("./a/b/Photo.JPG"), Some("image/jpeg"));
        assert_eq!(image_mime_from_ext("logo.svg"), Some("image/svg+xml"));
        assert_eq!(image_mime_from_ext("anim.webp"), Some("image/webp"));
        // Not images: plain docs and extension-less names.
        assert_eq!(image_mime_from_ext("design.md"), None);
        assert_eq!(image_mime_from_ext("plan"), None);
    }

    #[test]
    fn html_extensions_are_recognised_case_insensitively() {
        assert!(is_html_ext(Some("report.html")));
        assert!(is_html_ext(Some("./out/Dashboard.HTM")));
        // Not HTML: other docs, extension-less names, and stdin (no filename).
        assert!(!is_html_ext(Some("plan.md")));
        assert!(!is_html_ext(Some("notes")));
        assert!(!is_html_ext(None));
    }

    #[test]
    fn raster_magic_bytes_are_sniffed_but_text_is_not() {
        assert_eq!(
            image_mime_from_magic(b"\x89PNG\r\n\x1a\n....."),
            Some("image/png")
        );
        assert_eq!(
            image_mime_from_magic(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some("image/jpeg")
        );
        assert_eq!(image_mime_from_magic(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            image_mime_from_magic(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        // Markdown that merely contains an <svg> is text, never sniffed as image.
        assert_eq!(image_mime_from_magic(b"# Notes\n<svg>...</svg>\n"), None);
    }

    #[test]
    fn image_encoding_produces_a_data_uri_and_passes_text_through() {
        // A PNG by extension → a base64 data URI.
        let png = b"\x89PNG\r\n\x1a\nzzzz";
        let uri = encode_image_data_uri(Some("shot.png"), png)
            .unwrap()
            .expect("png embeds");
        assert!(uri.starts_with("data:image/png;base64,"));

        // Extension-less stdin still embeds via magic bytes.
        let uri = encode_image_data_uri(None, png)
            .unwrap()
            .expect("magic embeds");
        assert!(uri.starts_with("data:image/png;base64,"));

        // SVG (text) embeds by extension so it renders as an image, not source.
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'></svg>";
        let uri = encode_image_data_uri(Some("d.svg"), svg)
            .unwrap()
            .expect("svg embeds");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));

        // Non-image content is left for the text path.
        assert!(encode_image_data_uri(Some("notes.md"), b"# Hi")
            .unwrap()
            .is_none());
        assert!(encode_image_data_uri(None, b"plain text")
            .unwrap()
            .is_none());
    }

    #[test]
    fn embed_rejects_an_oversized_image() {
        let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(encode_image_data_uri(Some("x.png"), &big).is_err());
    }
}
