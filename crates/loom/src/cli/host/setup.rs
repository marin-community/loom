//! `loom setup` — the guided credential wizards.
//!
//! Interactive and daemon-less: these prompt at a terminal and write straight
//! into loom's sqlite settings, so no operation can stand in for them.

use super::config::{default_config_path, ConfigPathOpts};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use weaver_core::db::Db;

/// Subcommands under `loom setup` — the guided credential wizards.
#[derive(Subcommand)]
pub enum SetupCmd {
    /// Create the GitHub App loom uses, via GitHub's manifest flow.
    GithubApp(GithubAppOpts),
    /// Prompt for and store default-profile model-provider secrets.
    Secrets(SecretsOpts),
}

#[derive(Args)]
pub struct GithubAppOpts {
    /// loom's public base URL, e.g. `https://loom.team.dev` (`localhost:7878`
    /// for a local try-out). Becomes the App's homepage, webhook target
    /// (`{base_url}/api/github/webhook`), and OAuth-login callback base
    /// (`{base_url}/api/auth/github/callback`).
    #[arg(long)]
    base_url: String,
    /// The App's display name — must be unique across all of GitHub. Defaults
    /// to `loom-<host>`, derived from `--base-url`.
    #[arg(long)]
    name: Option<String>,
    /// Create the App under this GitHub organization instead of your personal
    /// account.
    #[arg(long)]
    org: Option<String>,
    /// The GitHub login approved to sign in first (`LOOM_OWNER_GITHUB`).
    /// Required with `--org`: an org install's App is owned by the org, but
    /// the first approved sign-in needs an individual login, which the org's
    /// own login isn't — prompted for interactively if omitted. Optional
    /// without `--org`, where it defaults to your own account (the one that
    /// confirms App creation).
    #[arg(long)]
    owner: Option<String>,
    /// Local port for the manifest-flow confirmation callback. `0` (default)
    /// picks a free port; pin one when you're tunnelling in to a remote host
    /// (e.g. `ssh -L 8765:localhost:8765 …`, then `--port 8765`).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// How long to wait for the browser confirmation, in seconds.
    #[arg(long, default_value = "300")]
    timeout: u64,
    /// Don't try to open a browser automatically — just print the confirmation
    /// page's URL.
    #[arg(long)]
    no_open: bool,
    #[command(flatten)]
    config: ConfigPathOpts,
}

#[derive(Args)]
pub struct SecretsOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
}

pub async fn run_setup(cmd: Option<SetupCmd>) -> Result<()> {
    match cmd {
        None => cmd_setup_init().await,
        Some(SetupCmd::GithubApp(opts)) => cmd_setup_github_app(opts).await,
        Some(SetupCmd::Secrets(opts)) => cmd_setup_secrets(opts).await,
    }
}

/// `loom setup` with no subcommand — the guided walkthrough. Its one hard
/// guarantee is a **bootstrap operator**: it always seeds one (live into the DB
/// and into `loom.toml`), so the instance can start and someone can sign in —
/// the interactive complement to [`crate::server::ensure_bootstrap_operator`]'s
/// boot guard. The GitHub App and agent-secret steps are offered but skippable,
/// and delegate to the same [`cmd_setup_github_app`]/[`cmd_setup_secrets`] the
/// subcommands use. A failure in an optional step is reported and the walkthrough
/// continues, so a browser timeout can't cost you the operator you just set up.
pub(crate) async fn cmd_setup_init() -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!(
            "loom setup needs an interactive terminal — run it directly (not piped or in CI). \
             For a non-interactive deploy, set LOOM_OWNER_GITHUB (and the other LOOM_* vars) \
             and run `loom config render-env` instead."
        );
    }
    let config_path = default_config_path();
    println!(
        "loom setup — I'll ask a few questions, write them to {}, and apply them to the",
        config_path.display()
    );
    println!("database so they take effect immediately.");
    println!();

    let db = crate::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;

    // Pre-fill each step's default from any existing config, so re-running the
    // wizard updates in place instead of restarting from scratch. The operator
    // login falls back to the seeded primary user when loom.toml has none yet.
    let existing_cfg = crate::loom_config::load(&config_path).ok();
    let prefill_owner = existing_cfg
        .as_ref()
        .and_then(|c| c.owner_github.clone())
        .or(crate::auth::primary_user(&db).await.ok().flatten());
    let prefill_base_url = existing_cfg
        .as_ref()
        .and_then(|c| c.domain.as_deref())
        .and_then(base_url_from_domain);

    // Step 1 — bootstrap operator (required). Without one, no one can sign in
    // and the daemon refuses to start, so this step cannot be skipped.
    println!("Step 1/4 · Bootstrap operator (required)");
    println!("  The GitHub login allowed to sign in first and approve everyone else.");
    let owner = loop {
        let login = prompt_line("GitHub login", prefill_owner.as_deref())?;
        if crate::github_trigger::valid_login(&login) {
            break login;
        }
        println!("  '{login}' isn't a valid GitHub login (letters, digits, and hyphens only).");
    };
    if crate::auth::get_user(&db, &owner).await?.is_none() {
        crate::auth::add_user(
            &db,
            &owner,
            Some(&owner),
            None,
            crate::auth::UserRole::Admin,
        )
        .await
        .with_context(|| format!("seeding the bootstrap operator '{owner}'"))?;
    }
    crate::loom_config::upsert(&config_path, &[("LOOM_OWNER_GITHUB", owner.as_str())])
        .context("writing the operator into loom.toml")?;
    println!("  ✓ '{owner}' can sign in and trigger sessions by commenting.");
    println!();

    // Step 2 — public URL.
    println!("Step 2/4 · Public URL");
    println!("  Where loom is reachable; localhost for a local try-out.");
    let base_url = prompt_line(
        "Base URL",
        prefill_base_url
            .as_deref()
            .or(Some("http://localhost:7878")),
    )?
    .trim_end_matches('/')
    .to_string();
    let domain = host_from_base_url(&base_url).to_string();
    crate::loom_config::upsert(&config_path, &[("LOOM_DOMAIN", domain.as_str())])
        .context("writing the domain into loom.toml")?;
    println!();

    // Step 3 — GitHub App (optional; opens a browser). Delegates to the same
    // wizard the subcommand uses, passing the operator so it stays consistent.
    println!("Step 3/4 · GitHub App (recommended — opens your browser)");
    // An App already on file turns this step into an update/re-install (the
    // create-vs-update choice itself is offered inside `cmd_setup_github_app`).
    let app_exists = existing_app(&db).await.is_some();
    if app_exists {
        println!("  A GitHub App is already configured — you can update or re-install it.");
    } else {
        println!("  Creates the App loom acts through (webhook, sign-in, per-repo tokens).");
    }
    let step3_prompt = if app_exists {
        "Review / update the GitHub App now?"
    } else {
        "Set up the GitHub App now?"
    };
    if prompt_yes_no(step3_prompt, true)? {
        let app_opts = GithubAppOpts {
            base_url: base_url.clone(),
            name: None,
            org: None,
            owner: Some(owner.clone()),
            port: 0,
            timeout: 300,
            no_open: false,
            config: ConfigPathOpts {
                config: config_path.clone(),
            },
        };
        if let Err(e) = cmd_setup_github_app(app_opts).await {
            println!("  ! GitHub App setup didn't complete: {e}");
            println!("  Retry later with `loom setup github-app --base-url {base_url}`.");
        }
    } else if app_exists {
        println!("  Left the existing App as-is.");
    } else {
        println!("  Skipped — set it up later with `loom setup github-app`.");
    }
    println!();

    // Step 4 — agent secrets. Same wizard the subcommand uses.
    println!("Step 4/4 · Agent secrets");
    if let Err(e) = cmd_setup_secrets(SecretsOpts {
        config: ConfigPathOpts {
            config: config_path.clone(),
        },
    })
    .await
    {
        println!("  ! Secrets step didn't complete: {e}");
        println!("  Retry later with `loom setup secrets`.");
    }
    println!();

    println!("Setup complete. Next: run `loom config render-env` to produce a deploy `.env`,");
    println!("then start the daemon (e.g. `docker compose up -d`).");
    Ok(())
}

/// Prompt (plain text) for one line, showing `default` in brackets and returning
/// it on a blank answer. A `None` default makes the answer required — it
/// re-prompts until non-empty.
pub(crate) fn prompt_line(label: &str, default: Option<&str>) -> Result<String> {
    use std::io::Write;
    loop {
        match default {
            Some(d) => print!("  {label} [{d}]: "),
            None => print!("  {label}: "),
        }
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .with_context(|| format!("reading {label}"))?;
        let value = input.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        println!("  (required)");
    }
}

/// Prompt yes/no, returning `true` for yes; a blank answer takes `default_yes`.
pub(crate) fn prompt_yes_no(label: &str, default_yes: bool) -> Result<bool> {
    use std::io::Write;
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("  {label} {hint}: ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading a yes/no answer")?;
    Ok(match input.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Prompt for one of `options` by number, returning the chosen 0-based index.
/// A blank answer takes `default` (also 0-based); an out-of-range answer
/// re-prompts.
pub(crate) fn prompt_choice(prompt: &str, options: &[&str], default: usize) -> Result<usize> {
    use std::io::Write;
    println!("  {prompt}");
    for (i, opt) in options.iter().enumerate() {
        let marker = if i == default { "  (default)" } else { "" };
        println!("    {}) {opt}{marker}", i + 1);
    }
    loop {
        print!("  Choice [{}]: ", default + 1);
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("reading a menu choice")?;
        let s = input.trim();
        if s.is_empty() {
            return Ok(default);
        }
        match s.parse::<usize>() {
            Ok(n) if (1..=options.len()).contains(&n) => return Ok(n - 1),
            _ => println!("  Enter a number from 1 to {}.", options.len()),
        }
    }
}

/// Open `url` in the operator's browser (best-effort via `xdg-open`), always
/// printing it first so a headless or SSH-tunnelled run can open it by hand.
pub(crate) fn open_browser(url: &str, intro: &str) {
    println!("{intro}");
    println!("  {url}");
    let _ = std::process::Command::new("xdg-open").arg(url).status();
}

/// The GitHub App already recorded in loom's settings, if any. `slug`/`org` may
/// be absent for an App created before setup began recording them
/// ([`crate::github_app::APP_SLUG_KEY`]).
pub struct ExistingApp {
    id: String,
    slug: Option<String>,
    org: Option<String>,
}

/// Read the configured App from the settings table. `None` when no App id is
/// stored on a fresh or incompletely configured instance.
pub(crate) async fn existing_app(db: &Db) -> Option<ExistingApp> {
    let nonempty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let id = nonempty(weaver_core::config::get(db, crate::github_app::APP_ID_KEY).await)?;
    Some(ExistingApp {
        id,
        slug: nonempty(weaver_core::config::get(db, crate::github_app::APP_SLUG_KEY).await),
        org: nonempty(weaver_core::config::get(db, crate::github_app::APP_OWNER_KEY).await),
    })
}

/// Present the update / re-install menu for an already-configured App. Returns
/// `true` when the operator chose to create a brand-new App; `false` when the
/// existing App was handled here (a page opened, or left untouched).
pub(crate) async fn offer_existing_app(app: &ExistingApp) -> Result<bool> {
    println!(
        "  A GitHub App is already configured (id {}{}).",
        app.id,
        app.slug
            .as_deref()
            .map(|s| format!(", {s}"))
            .unwrap_or_default()
    );
    let choice = prompt_choice(
        "What would you like to do?",
        &[
            "Update its permissions/settings on GitHub (opens the App's settings page)",
            "Install or re-install it on repositories (opens the install page)",
            "Create a new App to replace it",
            "Leave it as-is",
        ],
        3,
    )?;
    match choice {
        // Update permissions/settings. loom can't change GitHub App permissions
        // itself — the owner edits them in the UI, then each installation
        // re-approves — so this deep-links to the right page and says what to do.
        0 => match &app.slug {
            Some(slug) => {
                let url = crate::github_manifest::settings_url(slug, app.org.as_deref());
                open_browser(
                    &url,
                    "  Opening the App's settings. If nothing opens, visit:",
                );
                println!(
                    "  Adjust Repository permissions as needed (e.g. Pull requests: Read & \
                     write), Save, then accept the updated permissions on each installation."
                );
            }
            None => println!(
                "  This App's slug isn't on record (it predates slug capture). Open your App \
                 settings manually at https://github.com/settings/apps and edit it there."
            ),
        },
        // Install / re-install / adjust repo access. The install page also
        // surfaces any pending permission re-approval.
        1 => match &app.slug {
            Some(slug) => {
                let url = crate::github_manifest::install_url(slug);
                open_browser(&url, "  Opening the install page. If nothing opens, visit:");
            }
            None => println!(
                "  This App's slug isn't on record; find it at \
                 https://github.com/settings/apps and use its Install button."
            ),
        },
        2 => return Ok(true),
        _ => println!("  Left the existing App unchanged."),
    }
    Ok(false)
}

/// Reconstruct a base URL from a stored `LOOM_DOMAIN` for pre-filling the
/// wizard: `localhost` (with no port on record) maps back to the local default,
/// any real domain to `https://<domain>`. `None` for an empty domain.
pub(crate) fn base_url_from_domain(domain: &str) -> Option<String> {
    let d = domain.trim();
    if d.is_empty() {
        None
    } else if d == "localhost" || d.starts_with("localhost:") || d.starts_with("127.0.0.1") {
        Some("http://localhost:7878".to_string())
    } else {
        Some(format!("https://{d}"))
    }
}

/// `loom setup github-app` — the manifest-flow wizard. Talks to GitHub and to
/// Loom's sqlite database directly through its daemon-less setup path;
/// it does not need the loom daemon to be running.
pub(crate) async fn cmd_setup_github_app(opts: GithubAppOpts) -> Result<()> {
    let base_url = opts.base_url.trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        bail!("--base-url must be a full URL, e.g. https://loom.team.dev (got '{base_url}')");
    }
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| default_app_name(&base_url));

    let db = crate::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;

    // If an App is already configured, offer to update / re-install it rather
    // than silently registering a second one. Only fall through to the manifest
    // create flow when the operator explicitly chooses to replace it (or when
    // running non-interactively, preserving the historical create behavior).
    if let Some(app) = existing_app(&db).await {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            if !offer_existing_app(&app).await? {
                return Ok(());
            }
            println!();
            println!("Creating a new App to replace the existing one…");
        } else {
            eprintln!(
                "note: a GitHub App (id {}) is already configured; creating another.",
                app.id
            );
        }
    }

    // Which account owns the App: an explicit `--org`, else ask interactively
    // (defaulting to the personal account). A non-interactive run with no `--org`
    // stays personal, preserving the historical default for scripted setups.
    let org: Option<String> = match &opts.org {
        Some(o) => Some(o.clone()),
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                prompt_org()?
            } else {
                None
            }
        }
    };

    // An org-owned App needs an explicit individual owner: the manifest flow's
    // own confirming account (`conv.owner.login`, used below when this is `None`)
    // is the org itself for an org install, which isn't a usable
    // `LOOM_OWNER_GITHUB` — a fresh database with no owner seeded locks everyone
    // out (see `db::seed_owner`). Resolve before opening the callback listener
    // so configuration errors fail before the browser confirmation.
    let org_owner: Option<String> = match (
        &org,
        opts.owner
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(_), Some(o)) => Some(o.to_string()),
        (Some(org), None) => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                Some(prompt_owner(org)?)
            } else {
                bail!(
                    "--org {org} needs --owner <your-github-login> — an org install's App is \
                     owned by the org, but the first approved sign-in needs an individual \
                     login, which the org's own login isn't"
                );
            }
        }
        (None, owner) => owner.map(str::to_string),
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port))
        .await
        .with_context(|| format!("binding the local callback server on port {}", opts.port))?;
    let port = listener
        .local_addr()
        .context("reading the callback server's bound port")?
        .port();
    let redirect_url = format!("http://127.0.0.1:{port}/callback");

    let manifest = crate::github_manifest::manifest_json(&crate::github_manifest::ManifestInput {
        name: &name,
        base_url: &base_url,
        redirect_url: &redirect_url,
    });
    let state = crate::auth::random_state();
    let target = crate::github_manifest::create_url(org.as_deref(), &state);
    let html = crate::github_manifest::submission_html(&manifest, &target);
    // Served at `/` by the same listener that catches the `/callback` redirect
    // — no `file://` URL, so this works unmodified through an SSH tunnel or a
    // one-shot published Docker port when loom isn't running on the machine
    // whose browser you're using.
    let start_url = format!("http://127.0.0.1:{port}/");

    println!("App name:  {name}");
    println!(
        "Owner:     {}",
        org.as_deref().unwrap_or("your personal account")
    );
    println!("Webhook:   {base_url}/api/github/webhook");
    println!("Login:     {base_url}/api/auth/github/callback");
    println!();
    if opts.no_open {
        println!("Open this in a browser to confirm App creation:");
    } else {
        println!("Opening a browser to confirm App creation. If nothing opens, visit:");
        let _ = std::process::Command::new("xdg-open")
            .arg(&start_url)
            .status();
    }
    println!("  {start_url}");
    println!(
        "(Tunnelling from another machine? `ssh -L {port}:localhost:{port} …` then open the \
         same URL there.)"
    );
    println!();
    println!(
        "Waiting for the GitHub confirmation (timeout {}s)…",
        opts.timeout
    );
    let code = crate::github_manifest::run_local_server(
        listener,
        html,
        state,
        std::time::Duration::from_secs(opts.timeout),
    )
    .await?;

    println!("Exchanging the confirmation for credentials…");
    let conv = crate::github_manifest::convert(&code)
        .await
        .context("converting the manifest code into App credentials")?;

    println!();
    println!(
        "Created {} (id {}) under {}",
        conv.slug, conv.id, conv.owner.login
    );
    println!("  {}", conv.html_url);

    weaver_core::config::apply(
        &db,
        &[
            (
                crate::github_app::APP_ID_KEY.to_string(),
                Some(conv.id.to_string()),
            ),
            (
                crate::github_app::APP_PRIVATE_KEY_KEY.to_string(),
                Some(conv.pem.clone()),
            ),
            (
                crate::github_trigger::WEBHOOK_SECRET_KEY.to_string(),
                Some(conv.webhook_secret.clone()),
            ),
            (
                crate::auth::GH_CLIENT_ID_KEY.to_string(),
                Some(conv.client_id.clone()),
            ),
            (
                crate::auth::GH_CLIENT_SECRET_KEY.to_string(),
                Some(conv.client_secret.clone()),
            ),
            // Recorded (not runtime credentials) so a later `loom setup` can
            // deep-link to this App's GitHub settings/install pages to update it.
            (
                crate::github_app::APP_SLUG_KEY.to_string(),
                Some(conv.slug.clone()),
            ),
            (
                crate::github_app::APP_OWNER_KEY.to_string(),
                Some(org.clone().unwrap_or_default()),
            ),
        ],
    )
    .await
    .context("writing the App credentials into loom's settings")?;
    println!();
    println!(
        "Stored the App id, private key, webhook secret, and OAuth client into loom's \
         settings — live on the running daemon, no restart needed."
    );

    let domain = host_from_base_url(&base_url);
    let app_id = conv.id.to_string();
    // The individual who can sign in first (`LOOM_OWNER_GITHUB`): for a personal
    // install the confirming account (`conv.owner.login`); for an org install
    // `org_owner` (the org itself can't sign in, so an individual is required).
    let owner_login = org_owner.as_deref().unwrap_or(conv.owner.login.as_str());

    // Approve that individual so they can sign in and trigger sessions. Written
    // live to the running daemon here, and to loom.toml (`LOOM_OWNER_GITHUB`)
    // below for a fresh DB. Their triggers on any repo the App is installed on
    // auto-register it — so an org install needs no separate owner allowlist.
    // Add more people in Settings → People & security.
    if crate::auth::get_user(&db, owner_login).await?.is_none() {
        crate::auth::add_user(
            &db,
            owner_login,
            Some(owner_login),
            None,
            crate::auth::UserRole::Admin,
        )
        .await
        .context("approving the bootstrap operator")?;
    }
    println!(
        "Approved '{owner_login}' — they can sign in and trigger sessions. Add more in \
         Settings → People & security."
    );

    let updates: Vec<(&str, &str)> = vec![
        ("LOOM_GITHUB_APP_ID", app_id.as_str()),
        ("LOOM_GITHUB_APP_SLUG", conv.slug.as_str()),
        ("LOOM_GITHUB_APP_PRIVATE_KEY", conv.pem.as_str()),
        ("LOOM_GITHUB_WEBHOOK_SECRET", conv.webhook_secret.as_str()),
        ("LOOM_GITHUB_CLIENT_ID", conv.client_id.as_str()),
        ("LOOM_GITHUB_CLIENT_SECRET", conv.client_secret.as_str()),
        ("LOOM_DOMAIN", domain),
        ("LOOM_OWNER_GITHUB", owner_login),
    ];
    crate::loom_config::upsert(&opts.config.config, &updates)
        .context("writing the App credentials into loom.toml")?;
    println!(
        "Also wrote them, plus LOOM_DOMAIN and LOOM_OWNER_GITHUB ({owner_login}), to {} — run \
         `loom config render-env` to produce a deploy `.env` from it.",
        opts.config.config.display()
    );

    println!();
    println!("Next steps:");
    println!("  1. Install the App on the repos loom should act on:");
    println!(
        "       https://github.com/apps/{}/installations/new",
        conv.slug
    );
    println!("  2. Sign in at {base_url} with GitHub — the App's OAuth client now handles login.");
    Ok(())
}

/// The bare host from a `--base-url` like `https://loom.team.dev` or
/// `http://localhost:7878` — no scheme, no port. What `LOOM_DOMAIN` expects
/// (the Caddyfile in `deploy/standalone` templates it in directly).
pub(crate) fn host_from_base_url(base_url: &str) -> &str {
    base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':'])
        .next()
        .unwrap_or("loom")
}

/// A default App name derived from the host in `--base-url` (`loom-<host>`,
/// non-alphanumerics folded to `-`) — GitHub App names must be unique across
/// all of GitHub, so this is a starting point, not a guarantee.
pub(crate) fn default_app_name(base_url: &str) -> String {
    let host = host_from_base_url(base_url);
    let slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("loom-{slug}")
}

/// `loom setup secrets` — prompt for the paste-once agent secrets and store
/// them as operator environment variables (`crate::agent_env`), exported into
/// every session loom launches from then on.
pub(crate) async fn cmd_setup_secrets(opts: SecretsOpts) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!(
            "loom setup secrets needs an interactive terminal (hidden input for the \
             secrets you paste) — run it directly, not piped or in CI"
        );
    }
    let db = crate::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;
    // Which secrets are already stored, so the prompts can say a blank answer
    // keeps the existing value rather than clearing it.
    let existing_names: std::collections::HashSet<String> = crate::agent_env::pairs(&db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();

    println!("Paste-once secrets for the default profile (leave blank to skip).");
    let anthropic = prompt_secret(
        "ANTHROPIC_API_KEY",
        existing_names.contains("ANTHROPIC_API_KEY"),
    )?;
    let openai = prompt_secret("OPENAI_API_KEY", existing_names.contains("OPENAI_API_KEY"))?;

    if anthropic.is_none() && openai.is_none() {
        println!("nothing entered — existing values kept unchanged");
        return Ok(());
    }

    let mut stored = Vec::new();
    if let Some(v) = &anthropic {
        crate::agent_env::set(&db, "ANTHROPIC_API_KEY", v).await?;
        stored.push("ANTHROPIC_API_KEY");
    }
    if let Some(v) = &openai {
        crate::agent_env::set(&db, "OPENAI_API_KEY", v).await?;
        stored.push("OPENAI_API_KEY");
    }
    println!();
    println!(
        "Stored {} on the default profile — future sessions using that profile get them \
         (Settings → Agents & profiles in the web UI, or `loom settings env list`).",
        stored.join(", ")
    );
    let mut updates: Vec<(&str, &str)> = Vec::new();
    if let Some(v) = &anthropic {
        updates.push(("ANTHROPIC_API_KEY", v.as_str()));
    }
    if let Some(v) = &openai {
        updates.push(("OPENAI_API_KEY", v.as_str()));
    }
    crate::loom_config::upsert(&opts.config.config, &updates)
        .context("writing the paste-once secrets into loom.toml")?;
    println!(
        "Also wrote them to {} — run `loom config render-env` then restart the daemon (e.g. \
         `docker compose up -d`) to apply them.",
        opts.config.config.display()
    );
    Ok(())
}

/// Ask whether the GitHub App should be owned by an organization instead of the
/// operator's personal account, returning the org login (or `None` for a
/// personal App). An org-owned App is created under the org's own developer
/// settings; the individual `LOOM_OWNER_GITHUB` is still resolved separately
/// (see [`prompt_owner`]).
pub(crate) fn prompt_org() -> Result<Option<String>> {
    let choice = prompt_choice(
        "Who should own the GitHub App?",
        &[
            "Your personal account",
            "An organization (its members share the App)",
        ],
        0,
    )?;
    if choice == 0 {
        return Ok(None);
    }
    let org = loop {
        let login = prompt_line("Organization login", None)?;
        if crate::github_trigger::valid_login(&login) {
            break login;
        }
        println!("  '{login}' isn't a valid GitHub org login (letters, digits, and hyphens only).");
    };
    Ok(Some(org))
}

/// Prompt (plain, not hidden — a GitHub login isn't a secret) for the
/// individual owner login an `--org` install needs, since the org itself
/// can't be `LOOM_OWNER_GITHUB`.
pub(crate) fn prompt_owner(org: &str) -> Result<String> {
    use std::io::Write;
    print!(
        "The App will be owned by the {org} organization, but the first approved sign-in needs \
         your individual GitHub login (LOOM_OWNER_GITHUB) — enter it: "
    );
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading the owner login")?;
    let owner = input.trim().to_string();
    if owner.is_empty() {
        bail!("an owner GitHub login is required for an --org install");
    }
    Ok(owner)
}

/// Prompt for a secret without echoing it to the terminal. An empty answer
/// means "skip" (leave the current value, if any, alone). `already_set` annotates
/// the prompt so the operator knows a blank answer keeps the stored value.
pub(crate) fn prompt_secret(name: &str, already_set: bool) -> Result<Option<String>> {
    let hint = if already_set {
        " (already set — blank keeps it)"
    } else {
        ""
    };
    let value = rpassword::prompt_password(format!("{name}{hint}: "))
        .with_context(|| format!("reading {name}"))?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_base_url_strips_scheme_and_port() {
        assert_eq!(host_from_base_url("https://loom.team.dev"), "loom.team.dev");
        assert_eq!(host_from_base_url("http://localhost:7878"), "localhost");
        assert_eq!(
            host_from_base_url("https://loom.example.com/"),
            "loom.example.com"
        );
    }
    #[test]
    fn base_url_from_domain_reconstructs_the_wizard_default() {
        // A real domain → https; the stored LOOM_DOMAIN has no scheme.
        assert_eq!(
            base_url_from_domain("loom.team.dev").as_deref(),
            Some("https://loom.team.dev")
        );
        // localhost lost its port on the way to LOOM_DOMAIN → the local default.
        assert_eq!(
            base_url_from_domain("localhost").as_deref(),
            Some("http://localhost:7878")
        );
        assert_eq!(
            base_url_from_domain("127.0.0.1").as_deref(),
            Some("http://localhost:7878")
        );
        // Nothing stored → no pre-fill (caller falls back to its own default).
        assert_eq!(base_url_from_domain(""), None);
        assert_eq!(base_url_from_domain("   "), None);
    }
    #[test]
    fn default_app_name_folds_the_host_from_base_url() {
        assert_eq!(
            default_app_name("https://loom.team.dev"),
            "loom-loom-team-dev"
        );
        assert_eq!(default_app_name("http://localhost:7878"), "loom-localhost");
    }
}
