#![cfg(feature = "live-e2e")]

use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::process;
use tempfile::tempdir;

// ── Helpers ─────────────────────────────────────────────────────────

/// The staging API URL used for live E2E tests.
///
/// The production API's ADR bank is not yet populated, so tests must target
/// staging which has ADR data. Override with `ACTUAL_E2E_API_URL` if needed.
const STAGING_API_URL: &str = "https://api-service.api.staging.actual.ai";

fn e2e_api_url() -> String {
    std::env::var("ACTUAL_E2E_API_URL").unwrap_or_else(|_| STAGING_API_URL.to_string())
}

/// Preflight: skip all tests if Claude Code is not installed/authenticated.
fn require_claude_auth() {
    let output = process::Command::new("claude")
        .args(["auth", "status", "--json"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if !stdout.contains("\"loggedIn\": true") && !stdout.contains("\"loggedIn\":true") {
                panic!("Claude Code not authenticated. Run: claude auth login");
            }
        }
        _ => panic!(
            "Claude Code not found or not working. Install with: npm install -g @anthropic-ai/claude-code"
        ),
    }
}

/// Create a minimal TypeScript/Express project.
///
/// TypeScript + Express reliably matches ADRs from the staging API bank.
fn create_minimal_ts_project(dir: &std::path::Path) {
    fs::write(
        dir.join("package.json"),
        r#"{
  "name": "test-project",
  "version": "1.0.0",
  "dependencies": {
    "express": "^4.18.0",
    "typescript": "^5.0.0"
  }
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "strict": true,
    "outDir": "dist"
  }
}
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/index.ts"),
        r#"import express from 'express';

const app = express();
app.use(express.json());

app.get('/health', (_req, res) => {
  res.json({ ok: true });
});

app.listen(3000, () => {
  console.log('Server running on port 3000');
});
"#,
    )
    .unwrap();
}

/// Create a realistic TypeScript/Next.js project with multiple source files.
///
/// A richer fixture helps ensure the ADR count assertions hold against the
/// staging API.
fn create_realistic_ts_project(dir: &std::path::Path) {
    fs::write(
        dir.join("package.json"),
        r#"{
  "name": "sample-nextjs-app",
  "version": "1.0.0",
  "dependencies": {
    "next": "^15.0.0",
    "react": "^18.0.0",
    "react-dom": "^18.0.0",
    "typescript": "^5.0.0",
    "zod": "^3.0.0"
  },
  "devDependencies": {
    "@types/node": "^20.0.0",
    "@types/react": "^18.0.0",
    "jest": "^29.0.0"
  }
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "lib": ["dom", "dom.iterable", "esnext"],
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "jsx": "preserve"
  }
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("next.config.ts"),
        r#"import type { NextConfig } from 'next';

const config: NextConfig = {
  reactStrictMode: true,
};

export default config;
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(
        dir.join("app/page.tsx"),
        r#"export default function HomePage() {
  return <main><h1>Hello World</h1></main>;
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("app/layout.tsx"),
        r#"export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("app/api/users")).unwrap();
    fs::write(
        dir.join("app/api/users/route.ts"),
        r#"import { NextResponse } from 'next/server';
import { z } from 'zod';

const UserSchema = z.object({
  name: z.string().min(1),
  email: z.string().email(),
});

export async function POST(req: Request) {
  const body = await req.json();
  const result = UserSchema.safeParse(body);
  if (!result.success) {
    return NextResponse.json({ errors: result.error.flatten() }, { status: 400 });
  }
  return NextResponse.json({ user: result.data }, { status: 201 });
}
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::write(
        dir.join("lib/db.ts"),
        r#"// Database client module
export async function query<T>(sql: string, params: unknown[]): Promise<T[]> {
  // Placeholder for database queries
  void sql;
  void params;
  return [];
}
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(
        dir.join("tests/api.test.ts"),
        r#"describe('API routes', () => {
  it('returns 201 for valid user', () => {
    expect(true).toBe(true);
  });
});
"#,
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), "node_modules\n.next\ndist\n").unwrap();
}

fn init_git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {} failed", args.join(" "));
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
}

fn actual_cmd() -> Command {
    Command::from(cargo_bin_cmd!("actual"))
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn live_auth_check() {
    require_claude_auth();

    actual_cmd()
        .arg("auth")
        .assert()
        .success()
        .stdout(predicate::str::contains("uthenticat"));
}

#[test]
fn live_sync_no_tailor() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_ts_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--api-url",
            &e2e_api_url(),
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    let claude_md = dir.join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after sync");

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "missing managed start marker"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "missing managed end marker"
    );
    assert!(
        content.contains("<!-- version: 1 -->"),
        "missing version marker"
    );
    assert!(
        content.contains("<!-- adr-ids:"),
        "missing adr-ids metadata"
    );
    // At least one ADR heading
    assert!(content.contains("## "), "expected at least one ## heading");
}

#[test]
fn live_sync_realistic_project() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_realistic_ts_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--api-url",
            &e2e_api_url(),
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    let claude_md = dir.join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after sync");

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "missing managed start marker"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "missing managed end marker"
    );
    assert!(
        content.contains("<!-- version: 1 -->"),
        "missing version marker"
    );
    assert!(
        content.contains("<!-- adr-ids:"),
        "missing adr-ids metadata"
    );

    // A realistic Next.js project should match significantly more ADRs than the
    // minimal express project
    let heading_count = content.matches("## ").count();
    assert!(
        heading_count >= 3,
        "expected at least 3 ADR headings for a realistic project, got {heading_count}"
    );

    // Validate that adr-ids contains multiple entries
    let adr_ids_line = content
        .lines()
        .find(|l| l.contains("<!-- adr-ids:"))
        .expect("should have adr-ids line");
    let comma_count = adr_ids_line.matches(',').count();
    assert!(
        comma_count >= 2,
        "expected at least 3 ADR IDs (2+ commas) for realistic project, got {} commas in: {}",
        comma_count,
        adr_ids_line
    );

    // Managed content should be substantial for a realistic project
    let managed_start = content.find("<!-- managed:actual-start -->").unwrap();
    let managed_end = content.find("<!-- managed:actual-end -->").unwrap();
    let managed_len = managed_end - managed_start;
    assert!(
        managed_len > 500,
        "expected substantial managed content (>500 chars) for realistic project, got {managed_len}"
    );
}

#[test]
fn live_resync_preserves_user_content() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_ts_project(dir);
    init_git_repo(dir);

    // First sync
    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--api-url",
            &e2e_api_url(),
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    // Prepend user content before the managed section
    let claude_md = dir.join("CLAUDE.md");
    let existing = fs::read_to_string(&claude_md).unwrap();
    let with_user_content = format!("# My Custom Notes\n\nUser content here.\n\n{}", existing);
    fs::write(&claude_md, &with_user_content).unwrap();

    // Second sync
    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--api-url",
            &e2e_api_url(),
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("# My Custom Notes"),
        "user heading should be preserved"
    );
    assert!(
        content.contains("User content here."),
        "user content should be preserved"
    );
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "managed start marker should be present"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "managed end marker should be present"
    );
    assert!(
        content.contains("<!-- version: 2 -->"),
        "version should be incremented to 2"
    );
}

#[test]
fn live_dry_run_no_files() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_ts_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--dry-run",
            "--api-url",
            &e2e_api_url(),
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    assert!(
        !dir.join("CLAUDE.md").exists(),
        "CLAUDE.md should NOT exist after dry-run"
    );
}

#[test]
#[ignore = "requires LIVE_E2E_TAILOR=1; run with --ignored and set LIVE_E2E_TAILOR=1"]
fn live_sync_with_tailoring() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_ts_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--model",
            "haiku",
            "--api-url",
            &e2e_api_url(),
            "--max-budget-usd",
            "0.25",
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(180))
        .assert()
        .success();

    let claude_md = dir.join("CLAUDE.md");
    assert!(
        claude_md.exists(),
        "CLAUDE.md should exist after sync with tailoring"
    );

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "missing managed start marker"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "missing managed end marker"
    );
}
