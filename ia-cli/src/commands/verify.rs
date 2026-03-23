use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use color_print::cstr;
use console::style;

use ia_core::types::FileSource;
use ia_core::upload::checksum::{parse_checksums_multi, HashAlgorithm};
use ia_core::verify::{verify_item, VerifyInput, VerifyOpts, VerifyResult, VerifyStatus};
use ia_core::IaClient;

/// Verify that local files exist on archive.org with matching checksums.
///
/// Exits with code 0 if all files are verified, 1 if any file is missing
/// or mismatched. Designed for automation pipelines.
#[derive(Debug, Args)]
#[command(
    about = "Verify local files exist on archive.org with matching checksums",
    long_about = "Verify that local files exist on archive.org with matching checksums.\n\
        Exits with code 0 if all files are verified, 1 if any file is missing or mismatched.\n\
        Designed for automation pipelines where downstream steps must only run after\n\
        upload integrity is confirmed.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Verify specific files were uploaded</dim>\
         \n  <bold>$ ia verify my-item file1.pdf file2.pdf</bold>\
         \n\n  <dim># Verify an entire directory</dim>\
         \n  <bold>$ ia verify my-item ./local-files/</bold>\
         \n\n  <dim># Verify using pre-computed checksums (no local files needed)</dim>\
         \n  <bold>$ ia verify my-item --checksum-file md5sums.txt</bold>\
         \n\n  <dim># Use SHA-1 instead of MD5</dim>\
         \n  <bold>$ ia verify my-item ./files/ --checksum-type sha1</bold>\
         \n\n  <dim># Require exact filename match</dim>\
         \n  <bold>$ ia verify my-item ./files/ --match-names</bold>\
         \n\n  <dim># Only verify PDFs on the remote item</dim>\
         \n  <bold>$ ia verify my-item ./files/ --glob '*.pdf'</bold>\
         \n\n  <dim># Batch verify from upload spreadsheet</dim>\
         \n  <bold>$ ia verify --spreadsheet upload.csv</bold>\
         \n\n  <dim># Gate a script on successful verification</dim>\
         \n  <bold>$ ia verify my-item ./files/ -q && ./post-upload.sh</bold>\
         \n\n  <dim># Generate checksums, upload, then verify without re-hashing</dim>\
         \n  <bold>$ md5sum ./files/* > checksums.txt</bold>\
         \n  <bold>$ ia upload my-item ./files/</bold>\
         \n  <bold>$ ia verify my-item --checksum-file checksums.txt</bold>\n"
    ),
)]
pub struct VerifyArgs {
    /// Item identifier to verify against
    #[arg(required_unless_present = "spreadsheet")]
    pub identifier: Option<String>,

    /// Local files or directories to verify
    #[arg(required_unless_present_any = ["checksum_file", "spreadsheet"])]
    pub files: Vec<PathBuf>,

    /// Pre-computed checksum file (GNU md5sum/sha1sum/BSD formats)
    #[arg(long = "checksum-file", alias = "checksums")]
    pub checksum_file: Option<PathBuf>,

    /// Hash algorithm (auto-detected from checksum file if not specified)
    #[arg(long = "checksum-type", value_parser = parse_algorithm)]
    pub checksum_type: Option<HashAlgorithm>,

    /// Require filename match in addition to hash match
    #[arg(long)]
    pub match_names: bool,

    /// Filter which remote files to consider when matching
    #[arg(long)]
    pub glob: Option<String>,

    /// Filter remote files by IA format field (repeatable)
    #[arg(long)]
    pub format: Vec<String>,

    /// Filter remote files by source (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    pub source: Option<FileSource>,

    /// Batch verify from spreadsheet (CSV/TSV/XLSX/ODS/JSONL)
    #[arg(long, conflicts_with_all = ["identifier", "files"])]
    pub spreadsheet: Option<PathBuf>,

    /// Machine-readable JSONL output
    #[arg(long)]
    pub json: bool,
}

fn parse_algorithm(s: &str) -> std::result::Result<HashAlgorithm, String> {
    match s.to_lowercase().as_str() {
        "md5" => Ok(HashAlgorithm::Md5),
        "sha1" => Ok(HashAlgorithm::Sha1),
        "crc32" => Ok(HashAlgorithm::Crc32),
        _ => Err(format!("unknown algorithm '{s}' (valid: md5, sha1, crc32)")),
    }
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!(
            "unknown source: {s} (expected: original, derivative, metadata)"
        )),
    }
}

pub async fn run(client: &IaClient, args: VerifyArgs, quiet: u8, jobs: usize) -> Result<()> {
    if let Some(spreadsheet_path) = &args.spreadsheet {
        return run_spreadsheet(client, &args, spreadsheet_path, quiet, jobs).await;
    }

    let identifier = args
        .identifier
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("identifier is required"))?;

    let (inputs, algorithm) = build_inputs(&args)?;

    let opts = VerifyOpts {
        algorithm,
        checksums: None,
        match_names: args.match_names,
        glob: args.glob.clone(),
        formats: args.format.clone(),
        source: args.source.clone(),
    };

    let results = verify_item(client, identifier, &inputs, &opts).await?;
    let has_failures = results.iter().any(|r| r.status != VerifyStatus::Verified);

    print_results(identifier, &results, &args, quiet);

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

// ─── Input building ──────────────────────────────────────────────────────────

fn build_inputs(args: &VerifyArgs) -> Result<(Vec<VerifyInput>, HashAlgorithm)> {
    let mut inputs = Vec::new();
    // None = auto-detect, Some = user explicitly set
    let explicit_algorithm = args.checksum_type.clone();
    let mut algorithm = explicit_algorithm.clone().unwrap_or(HashAlgorithm::Md5);

    // Load checksum file if provided
    if let Some(path) = &args.checksum_file {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read checksum file: {e}"))?;

        let checksums = parse_checksums_multi(&content, explicit_algorithm.clone());

        // Auto-detect algorithm from first entry if user didn't explicitly set
        if args.checksum_type.is_none() {
            if let Some(entry) = checksums.values().next() {
                algorithm = entry.algorithm.clone();
            }
        }

        for (filename, entry) in &checksums {
            inputs.push(VerifyInput::Hash {
                filename: filename.clone(),
                hash: entry.hash.clone(),
            });
        }
    }

    // Expand file/directory arguments (recursive, skips dotfiles/symlinks)
    if !args.files.is_empty() {
        for path in &args.files {
            if !path.exists() {
                anyhow::bail!("file not found: {}", path.display());
            }
        }
        let expanded = ia_core::fs_util::expand_files(&args.files)?;
        for path in expanded {
            inputs.push(VerifyInput::LocalFile(path));
        }
    }

    if inputs.is_empty() {
        anyhow::bail!("no files to verify — provide files, directories, or --checksum-file");
    }

    Ok((inputs, algorithm))
}

// ─── Console output ──────────────────────────────────────────────────────────

fn print_results(identifier: &str, results: &[VerifyResult], args: &VerifyArgs, quiet: u8) {
    if args.json {
        print_json_results(results);
        return;
    }

    if quiet >= 1 {
        return;
    }

    let verified = results
        .iter()
        .filter(|r| r.status == VerifyStatus::Verified)
        .count();
    let missing = results
        .iter()
        .filter(|r| r.status == VerifyStatus::Missing)
        .count();
    let mismatched = results
        .iter()
        .filter(|r| r.status == VerifyStatus::Mismatch)
        .count();
    let errors = results
        .iter()
        .filter(|r| r.status == VerifyStatus::Error)
        .count();
    let failures = missing + mismatched + errors;

    eprintln!(
        "{} {}  ({} files)",
        style("▸").cyan(),
        style(identifier).bold(),
        results.len()
    );

    for r in results {
        print_result_line(r);
    }

    // Summary line
    eprintln!();
    if failures > 0 {
        eprintln!(
            "{}  {} {}",
            identifier,
            failures,
            if failures == 1 { "error" } else { "errors" }
        );
    } else {
        eprintln!("{identifier}");
    }

    let mut parts = Vec::new();
    if verified > 0 {
        parts.push(format!(
            "{} {} verified",
            style("\u{2713}").green(),
            verified
        ));
    }
    if missing > 0 {
        parts.push(format!("{} {} missing", style("\u{2717}").red(), missing));
    }
    if mismatched > 0 {
        parts.push(format!(
            "{} {} mismatch",
            style("\u{2717}").red(),
            mismatched
        ));
    }
    if errors > 0 {
        parts.push(format!("{} {} errors", style("\u{2717}").red(), errors));
    }
    eprintln!("  {}", parts.join(" \u{00b7} "));
}

fn print_result_line(r: &VerifyResult) {
    match r.status {
        VerifyStatus::Verified => {
            let size_str = r.bytes.map(crate::output::format_bytes).unwrap_or_default();
            let hash_short = r
                .local_hash
                .as_deref()
                .map(|h| if h.len() > 8 { &h[..8] } else { h })
                .unwrap_or("");
            let remote_note = if r.remote_key.as_deref() != Some(&*r.local_file) {
                r.remote_key
                    .as_deref()
                    .map(|k| format!("  (remote: {k})"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            eprintln!(
                "  {} {:<20} {:>10}  {}\u{2026}{}",
                style("\u{2713}").green(),
                r.local_file,
                size_str,
                hash_short,
                style(remote_note).dim(),
            );
        }
        VerifyStatus::Missing => {
            eprintln!(
                "  {} {:<20} {}",
                style("\u{2717}").red(),
                r.local_file,
                style("no matching hash on remote").dim(),
            );
        }
        VerifyStatus::Mismatch => {
            let local_h = r.local_hash.as_deref().unwrap_or("?");
            let remote_h = r.remote_hash.as_deref().unwrap_or("?");
            eprintln!(
                "  {} {:<20} {} mismatch (local: {}\u{2026} remote: {}\u{2026})",
                style("\u{2717}").red(),
                r.local_file,
                r.algorithm,
                &local_h[..local_h.len().min(8)],
                &remote_h[..remote_h.len().min(8)],
            );
        }
        VerifyStatus::Error => {
            let detail = r.detail.as_deref().unwrap_or("unknown error");
            eprintln!(
                "  {} {:<20} {}",
                style("\u{2717}").red(),
                r.local_file,
                style(detail).dim(),
            );
        }
    }
}

fn print_json_results(results: &[VerifyResult]) {
    for r in results {
        let json = match serde_json::to_string(r) {
            Ok(j) => j,
            Err(e) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"error": {"code": "serialization_error", "message": e.to_string()}})
                );
                continue;
            }
        };
        match r.status {
            VerifyStatus::Error => eprintln!("{json}"),
            VerifyStatus::Verified | VerifyStatus::Missing | VerifyStatus::Mismatch => {
                println!("{json}");
            }
        }
    }
}

// ─── Spreadsheet handler ─────────────────────────────────────────────────────

async fn run_spreadsheet(
    client: &IaClient,
    args: &VerifyArgs,
    spreadsheet_path: &std::path::Path,
    quiet: u8,
    jobs: usize,
) -> Result<()> {
    use std::collections::HashSet;

    let records = ia_core::spreadsheet::read_spreadsheet(spreadsheet_path)?;
    if records.is_empty() {
        anyhow::bail!("spreadsheet is empty");
    }

    // Validate that the 'file' column exists (check first record)
    let first = &records[0];
    if !first.1.contains_key("file") {
        anyhow::bail!("spreadsheet must have a 'file' column");
    }

    // Determine which hash column to use
    let algorithm = args.checksum_type.clone().unwrap_or(HashAlgorithm::Md5);
    let hash_col = algorithm.field_name();

    // Group records by identifier, deduplicate identical identifier+file pairs
    let mut by_identifier: Vec<(String, Vec<VerifyInput>)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (identifier, fields) in &records {
        let file_val = fields
            .get("file")
            .ok_or_else(|| anyhow::anyhow!("row missing 'file' column"))?;

        // Deduplicate identical identifier+file rows
        if !seen.insert((identifier.clone(), file_val.clone())) {
            continue;
        }

        let input = if let Some(hash) = fields.get(hash_col).filter(|h| !h.is_empty()) {
            // Use pre-computed hash from spreadsheet column
            VerifyInput::Hash {
                filename: file_val.clone(),
                hash: hash.clone(),
            }
        } else {
            // Need local file
            let path = PathBuf::from(file_val);
            if !path.exists() {
                anyhow::bail!(
                    "file not found: {} \u{2014} provide --checksum-file or add {} column to spreadsheet",
                    path.display(),
                    hash_col
                );
            }
            VerifyInput::LocalFile(path)
        };

        if let Some(group) = by_identifier.iter_mut().find(|(id, _)| id == identifier) {
            group.1.push(input);
        } else {
            by_identifier.push((identifier.clone(), vec![input]));
        }
    }

    // Build opts
    let opts = VerifyOpts {
        algorithm,
        checksums: None,
        match_names: args.match_names,
        glob: args.glob.clone(),
        formats: args.format.clone(),
        source: args.source.clone(),
    };

    let mut all_results: Vec<(String, Vec<VerifyResult>)> = Vec::new();
    let mut has_failures = false;

    // Process items with concurrency, preserving spreadsheet order
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut handles = Vec::new();

    for (identifier, inputs) in by_identifier {
        let client = client.clone();
        let opts = opts.clone();
        let sem = semaphore.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.map_err(|e| anyhow::anyhow!("{e}"))?;
            let results = verify_item(&client, &identifier, &inputs, &opts).await?;
            Ok::<_, anyhow::Error>((identifier, results))
        }));
    }

    // Collect in spawn order (preserves spreadsheet identifier order)
    for handle in handles {
        let (identifier, results) = handle.await??;
        if results.iter().any(|r| r.status != VerifyStatus::Verified) {
            has_failures = true;
        }
        all_results.push((identifier, results));
    }

    // Print output
    print_batch_results(&all_results, args, quiet);

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

fn print_batch_results(all_results: &[(String, Vec<VerifyResult>)], args: &VerifyArgs, quiet: u8) {
    if args.json {
        for (_, results) in all_results {
            print_json_results(results);
        }
        return;
    }

    if quiet >= 1 {
        return;
    }

    let mut total_verified = 0usize;
    let mut total_missing = 0usize;
    let mut total_mismatch = 0usize;
    let mut total_errors = 0usize;
    let mut total_files = 0usize;

    for (identifier, results) in all_results {
        print_results(identifier, results, args, quiet);
        total_files += results.len();
        for r in results {
            match r.status {
                VerifyStatus::Verified => total_verified += 1,
                VerifyStatus::Missing => total_missing += 1,
                VerifyStatus::Mismatch => total_mismatch += 1,
                VerifyStatus::Error => total_errors += 1,
            }
        }
        eprintln!();
    }

    // Batch summary
    eprintln!(
        "Verified {} items, {} files",
        all_results.len(),
        total_files
    );
    let mut parts = Vec::new();
    if total_verified > 0 {
        parts.push(format!(
            "{} {} verified",
            style("\u{2713}").green(),
            total_verified
        ));
    }
    if total_missing > 0 {
        parts.push(format!(
            "{} {} missing",
            style("\u{2717}").red(),
            total_missing
        ));
    }
    if total_mismatch > 0 {
        parts.push(format!(
            "{} {} mismatch",
            style("\u{2717}").red(),
            total_mismatch
        ));
    }
    if total_errors > 0 {
        parts.push(format!(
            "{} {} errors",
            style("\u{2717}").red(),
            total_errors
        ));
    }
    eprintln!("  {}", parts.join(" \u{00b7} "));
}
