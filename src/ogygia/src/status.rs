use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use clap::{Args, Subcommand};

const STATE_PATHS: [(&str, &str, &str); 3] = [
    ("⚡", "Current system", "/run/current-system"),
    ("🥾", "Booted system", "/run/booted-system"),
    ("🔜", "Next boot system", "/nix/var/nix/profiles/system"),
];

#[derive(Subcommand)]
pub enum Command {
    /// Show build commits for the local host
    Status,
    /// Compare two NixOS system closures
    DiffClosures(DiffClosuresArgs),
}

#[derive(Args)]
pub struct DiffClosuresArgs {
    /// First NixOS system store path
    path1: PathBuf,

    /// Second NixOS system store path
    path2: PathBuf,

    /// Ignore the ogygia managed build-revision when comparing
    #[arg(long)]
    ignore_build_revision: bool,

    /// Exit with code 1 if closures differ, 0 if identical
    #[arg(long)]
    exit_code: bool,
}

impl Command {
    pub fn run(&self) {
        match self {
            Command::Status => show_revisions(),
            Command::DiffClosures(args) => diff_closures(args),
        }
    }
}

fn show_revisions() {
    for (emoji, label, path) in STATE_PATHS {
        let revision = format_revision(Path::new(path));
        println!("{} {:18} {}", emoji, label, revision);
    }
}

fn format_revision(base_path: &Path) -> String {
    let revision_path = base_path.join("sw/share/ogygia/build-revision");

    match std::fs::read_to_string(&revision_path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.len() > 12 {
                trimmed[..12].to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "unknown".into(),
        Err(error) => format!("error reading ({}): {}", revision_path.display(), error),
    }
}

fn diff_closures(args: &DiffClosuresArgs) {
    // Validate paths exist
    if !args.path1.exists() {
        eprintln!("Error: Path does not exist: {}", args.path1.display());
        std::process::exit(2);
    }
    if !args.path2.exists() {
        eprintln!("Error: Path does not exist: {}", args.path2.display());
        std::process::exit(2);
    }

    // If paths are identical, they're the same closure
    if args.path1 == args.path2 {
        if args.exit_code {
            std::process::exit(0);
        }
        return;
    }

    // Build dependency trees for both closures
    let tree1 = match build_dependency_tree(&args.path1, args.ignore_build_revision) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error building tree for {}: {}", args.path1.display(), e);
            std::process::exit(2);
        }
    };

    let tree2 = match build_dependency_tree(&args.path2, args.ignore_build_revision) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error building tree for {}: {}", args.path2.display(), e);
            std::process::exit(2);
        }
    };

    // Compare the two trees and annotate differences
    let diff_tree = compare_trees(&tree1, &tree2);

    // Check if there are any differences
    let has_changes = tree_has_changes(&diff_tree);

    if !has_changes {
        if args.exit_code {
            std::process::exit(0);
        }
        return;
    }

    // Display the diff tree
    eprintln!("{} → {}", strip_store_prefix(&tree1.path), strip_store_prefix(&tree2.path));
    render_tree(&diff_tree, "", true);

    // Exit with code 1 if differences found and --exit-code is set
    if args.exit_code {
        std::process::exit(1);
    }
}

#[derive(Debug, Clone, PartialEq)]
enum NodeStatus {
    Unchanged,
    Added,
    Removed,
    Changed(String), // Contains the "other" path for changed nodes
}

#[derive(Debug, Clone)]
struct TreeNode {
    path: String,
    children: Vec<TreeNode>,
    status: NodeStatus,
}

fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

fn should_filter_path(path: &str, ignore_build_revision: bool) -> bool {
    if !ignore_build_revision {
        return false;
    }
    path.contains("build-revision")
}

fn get_references(path: &Path) -> Result<Vec<String>, String> {
    let output = ProcessCommand::new("nix-store")
        .arg("--query")
        .arg("--references")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to execute nix-store: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("nix-store command failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|s| s.to_string())
        .collect())
}

fn build_dependency_tree(root: &Path, ignore_build_revision: bool) -> Result<TreeNode, String> {
    let root_str = root.to_string_lossy().to_string();
    let mut visited = HashSet::new();
    build_tree_recursive(&root_str, &mut visited, ignore_build_revision)
}

fn build_tree_recursive(
    path: &str,
    visited: &mut HashSet<String>,
    ignore_build_revision: bool,
) -> Result<TreeNode, String> {
    // Avoid cycles by tracking visited nodes
    if visited.contains(path) {
        return Ok(TreeNode {
            path: path.to_string(),
            children: Vec::new(),
            status: NodeStatus::Unchanged,
        });
    }
    visited.insert(path.to_string());

    let references = get_references(Path::new(path))?;
    let mut children = Vec::new();

    for ref_path in references {
        // Skip self-references and filtered paths
        if ref_path == path || should_filter_path(&ref_path, ignore_build_revision) {
            continue;
        }

        children.push(build_tree_recursive(&ref_path, visited, ignore_build_revision)?);
    }

    // Sort children by path for consistent output
    children.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(TreeNode {
        path: path.to_string(),
        children,
        status: NodeStatus::Unchanged,
    })
}

fn compare_trees(tree1: &TreeNode, tree2: &TreeNode) -> TreeNode {
    // Build maps of all paths in both trees
    let mut paths1 = HashMap::new();
    let mut paths2 = HashMap::new();
    collect_paths(tree1, &mut paths1);
    collect_paths(tree2, &mut paths2);

    // Start comparison from tree2's root (the "new" tree)
    compare_nodes(tree2, &paths1, &paths2)
}

fn collect_paths(node: &TreeNode, map: &mut HashMap<String, TreeNode>) {
    map.insert(node.path.clone(), node.clone());
    for child in &node.children {
        collect_paths(child, map);
    }
}

fn compare_nodes(
    node: &TreeNode,
    paths1: &HashMap<String, TreeNode>,
    paths2: &HashMap<String, TreeNode>,
) -> TreeNode {
    let status = if !paths1.contains_key(&node.path) {
        NodeStatus::Added
    } else if paths1.contains_key(&node.path) && paths2.contains_key(&node.path) {
        // Check if children differ
        let node1 = &paths1[&node.path];
        let children1: HashSet<_> = node1.children.iter().map(|c| &c.path).collect();
        let children2: HashSet<_> = node.children.iter().map(|c| &c.path).collect();

        if children1 == children2 {
            NodeStatus::Unchanged
        } else {
            NodeStatus::Unchanged // Still mark as unchanged at this level, children will show changes
        }
    } else {
        NodeStatus::Unchanged
    };

    let mut children: Vec<TreeNode> = node
        .children
        .iter()
        .map(|child| compare_nodes(child, paths1, paths2))
        .collect();

    // Add removed children (present in tree1 but not in tree2)
    if let Some(node1) = paths1.get(&node.path) {
        for child1 in &node1.children {
            if !paths2.contains_key(&child1.path) {
                let mut removed_child = child1.clone();
                removed_child.status = NodeStatus::Removed;
                mark_subtree_removed(&mut removed_child);
                children.push(removed_child);
            }
        }
    }

    children.sort_by(|a, b| a.path.cmp(&b.path));

    TreeNode {
        path: node.path.clone(),
        children,
        status,
    }
}

fn mark_subtree_removed(node: &mut TreeNode) {
    node.status = NodeStatus::Removed;
    for child in &mut node.children {
        mark_subtree_removed(child);
    }
}

fn tree_has_changes(node: &TreeNode) -> bool {
    if node.status != NodeStatus::Unchanged {
        return true;
    }
    node.children.iter().any(tree_has_changes)
}

fn render_tree(node: &TreeNode, prefix: &str, is_last: bool) {
    let (symbol, path_display) = match &node.status {
        NodeStatus::Unchanged => ("  ", strip_store_prefix(&node.path).to_string()),
        NodeStatus::Added => ("+ ", strip_store_prefix(&node.path).to_string()),
        NodeStatus::Removed => ("- ", strip_store_prefix(&node.path).to_string()),
        NodeStatus::Changed(other) => (
            "~ ",
            format!("{} → {}", strip_store_prefix(&node.path), strip_store_prefix(other)),
        ),
    };

    // Only print if not the root (we already printed the root header)
    if !prefix.is_empty() {
        let branch = if is_last { "└─ " } else { "├─ " };
        eprintln!("{}{}{}{}", prefix, branch, symbol, path_display);
    }

    // Render children
    let child_prefix = if prefix.is_empty() {
        String::new()
    } else if is_last {
        format!("{}   ", prefix)
    } else {
        format!("{}│  ", prefix)
    };

    let child_count = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let is_last_child = i == child_count - 1;
        render_tree(child, &child_prefix, is_last_child);
    }
}
