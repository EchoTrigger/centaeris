use std::path::Path;

use tree_sitter::{Node, Parser};

const HOME_ROOT_MARKER: char = '\u{e000}';
const DYNAMIC_EXPANSION_MARKER: char = '\u{e001}';
const LITERAL_GLOB_MARKER: char = '\u{e002}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProtectedRoot {
    System,
    WorkingDirectory,
    Home,
    HomeMetadata,
}

impl ProtectedRoot {
    pub(super) fn logical_path(self) -> &'static str {
        match self {
            Self::System => "/",
            Self::WorkingDirectory => "$CWD",
            Self::Home => "$HOME",
            Self::HomeMetadata => "$HOME/.centaeris",
        }
    }
}

pub(super) fn directly_recursively_deleted_protected_root(
    command: &str,
    command_cwd: Option<&str>,
    execution_cwd: Option<&Path>,
) -> Result<Option<ProtectedRoot>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|error| format!("load Bash deletion-guard grammar failed: {error}"))?;
    let tree = parser
        .parse(command, None)
        .ok_or_else(|| "Bash deletion-guard parser returned no syntax tree".to_string())?;

    // Invalid Bash is left to the real shell so its native syntax error remains
    // the ToolResult. A syntactically valid direct rm call is inspected below.
    if tree.root_node().has_error() {
        return Ok(None);
    }

    let logical_cwd = logical_cwd(command_cwd, execution_cwd);
    inspect_node(tree.root_node(), command.as_bytes(), logical_cwd.as_deref())
}

fn inspect_node(
    node: Node<'_>,
    source: &[u8],
    logical_cwd: Option<&str>,
) -> Result<Option<ProtectedRoot>, String> {
    // A function definition does not execute its body. Invoking the function is
    // an indirect deletion form and deliberately outside this direct-call gate.
    if node.kind() == "function_definition" {
        return Ok(None);
    }

    if node.kind() == "command" {
        if let Some(root) = inspect_command(node, source, logical_cwd)? {
            return Ok(Some(root));
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(root) = inspect_node(child, source, logical_cwd)? {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

fn inspect_command(
    node: Node<'_>,
    source: &[u8],
    logical_cwd: Option<&str>,
) -> Result<Option<ProtectedRoot>, String> {
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(None);
    };
    let command_name = normalized_node_text(name_node, source)?;
    let mut cursor = node.walk();
    let arguments = node
        .children_by_field_name("argument", &mut cursor)
        .map(|argument| normalized_node_text(argument, source))
        .collect::<Result<Vec<_>, _>>()?;
    let Some((executable, arguments)) = unwrap_transparent_command(command_name, arguments) else {
        return Ok(None);
    };
    if executable_basename(executable.as_str()) != "rm" {
        return Ok(None);
    }

    inspect_rm_arguments(arguments.as_slice(), logical_cwd)
}

fn unwrap_transparent_command(
    mut executable: String,
    mut arguments: Vec<String>,
) -> Option<(String, Vec<String>)> {
    loop {
        match executable_basename(executable.as_str()) {
            "command" => {
                if arguments
                    .iter()
                    .take_while(|argument| argument.starts_with('-'))
                    .any(|argument| argument.contains('v') || argument.contains('V'))
                {
                    return None;
                }
                let command_index = arguments
                    .iter()
                    .position(|argument| argument == "--" || !argument.starts_with('-'))?;
                let command_index = if arguments[command_index] == "--" {
                    command_index + 1
                } else {
                    command_index
                };
                executable = arguments.get(command_index)?.clone();
                arguments = arguments.into_iter().skip(command_index + 1).collect();
            }
            "env" => {
                let command_index = env_command_index(arguments.as_slice())?;
                executable = arguments.get(command_index)?.clone();
                arguments = arguments.into_iter().skip(command_index + 1).collect();
            }
            _ => return Some((executable, arguments)),
        }
    }
}

fn env_command_index(arguments: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument == "--" {
            return (index + 1 < arguments.len()).then_some(index + 1);
        }
        if is_environment_assignment(argument) {
            index += 1;
            continue;
        }
        if matches!(
            argument,
            "-i" | "--ignore-environment" | "-0" | "--null" | "-v" | "--debug"
        ) {
            index += 1;
            continue;
        }
        if matches!(
            argument,
            "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
        ) {
            index += 2;
            continue;
        }
        if argument.starts_with("--unset=")
            || argument.starts_with("--chdir=")
            || argument.starts_with("--split-string=")
        {
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            return None;
        }
        return Some(index);
    }
    None
}

fn is_environment_assignment(value: &str) -> bool {
    let Some((name, _)) = value.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn inspect_rm_arguments(
    arguments: &[String],
    logical_cwd: Option<&str>,
) -> Result<Option<ProtectedRoot>, String> {
    let mut recursive = false;
    let mut options_ended = false;
    let mut targets = Vec::new();

    for argument in arguments {
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && is_recursive_rm_option(argument) {
            recursive = true;
            continue;
        }
        if !options_ended && argument.starts_with('-') && argument != "-" {
            continue;
        }
        targets.push(argument.as_str());
    }

    if !recursive {
        return Ok(None);
    }
    for target in targets {
        if let Some(root) = classify_protected_target(target, logical_cwd)? {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

fn is_recursive_rm_option(argument: &str) -> bool {
    argument == "--recursive"
        || (argument.starts_with('-')
            && !argument.starts_with("--")
            && argument[1..]
                .chars()
                .any(|character| matches!(character, 'r' | 'R')))
}

fn classify_protected_target(
    target: &str,
    logical_cwd: Option<&str>,
) -> Result<Option<ProtectedRoot>, String> {
    if target.contains(DYNAMIC_EXPANSION_MARKER) {
        return Ok(None);
    }
    if let Some(rest) = target.strip_prefix(HOME_ROOT_MARKER) {
        return Ok(if covers_symbolic_root(rest) {
            Some(ProtectedRoot::Home)
        } else if covers_home_metadata(rest) {
            Some(ProtectedRoot::HomeMetadata)
        } else {
            None
        });
    }
    if target.contains(HOME_ROOT_MARKER) {
        return Ok(None);
    }

    let path = analyze_path_target(target, logical_cwd)?;
    let Some(path) = path else {
        return Ok(None);
    };
    if let Some(glob) = path.direct_glob.as_deref() {
        if path.parent == "/" && glob_covers_all_children(glob) {
            return Ok(Some(ProtectedRoot::System));
        }
        if is_shell_drive_root(path.parent.as_str()) && glob_covers_all_children(glob) {
            return Ok(Some(ProtectedRoot::System));
        }
        if logical_cwd.is_some_and(|cwd| path.parent == cwd) && glob_covers_all_children(glob) {
            return Ok(Some(ProtectedRoot::WorkingDirectory));
        }
        if let Some((working_parent, working_name)) =
            logical_cwd.and_then(|cwd| cwd.rsplit_once('/'))
        {
            if path.parent == working_parent && glob_matches_literal(glob, working_name) {
                return Ok(Some(ProtectedRoot::WorkingDirectory));
            }
        }
        return Ok(None);
    }

    if path.parent == "/" || is_shell_drive_root(path.parent.as_str()) {
        return Ok(Some(ProtectedRoot::System));
    }
    Ok(logical_cwd
        .filter(|cwd| path.parent == *cwd)
        .map(|_| ProtectedRoot::WorkingDirectory))
}

fn covers_symbolic_root(rest: &str) -> bool {
    let trimmed = rest.trim_matches('/');
    trimmed.is_empty() || trimmed == "." || trimmed == ".." || glob_covers_all_children(trimmed)
}

fn covers_home_metadata(rest: &str) -> bool {
    let trimmed = rest.trim_matches('/');
    trimmed == ".centaeris"
        || trimmed == ".centaeris/."
        || matches!(
            trimmed.strip_prefix(".centaeris/"),
            Some("*") | Some("**") | Some(".*") | Some(".**") | Some("{*,.*}") | Some("{.*,*}")
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnalyzedPath {
    parent: String,
    direct_glob: Option<String>,
}

fn analyze_path_target(
    target: &str,
    logical_cwd: Option<&str>,
) -> Result<Option<AnalyzedPath>, String> {
    if target.is_empty() || target.contains(LITERAL_GLOB_MARKER) {
        return Ok(None);
    }
    let normalized_target = normalized_shell_path(target);
    let mut components = if normalized_target.starts_with('/') {
        Vec::new()
    } else {
        let Some(cwd) = logical_cwd else {
            return Ok(None);
        };
        path_components(cwd)
    };
    let target_components = normalized_target.split('/').collect::<Vec<_>>();
    for (index, component) in target_components.iter().enumerate() {
        if component.is_empty() || *component == "." {
            continue;
        }
        if *component == ".." {
            components.pop();
            continue;
        }
        if contains_active_glob(component) {
            let has_trailing_component = target_components[index + 1..]
                .iter()
                .any(|item| !item.is_empty() && *item != ".");
            if has_trailing_component {
                return Ok(None);
            }
            return Ok(Some(AnalyzedPath {
                parent: components_to_path(components.as_slice()),
                direct_glob: Some((*component).to_string()),
            }));
        }
        components.push((*component).to_string());
    }
    Ok(Some(AnalyzedPath {
        parent: components_to_path(components.as_slice()),
        direct_glob: None,
    }))
}

fn contains_active_glob(component: &str) -> bool {
    component
        .chars()
        .any(|character| matches!(character, '*' | '?' | '['))
}

fn glob_covers_all_children(component: &str) -> bool {
    matches!(component, "*" | "**" | ".*" | ".**" | "{*,.*}" | "{.*,*}")
}

fn glob_matches_literal(pattern: &str, literal: &str) -> bool {
    let pattern = pattern.as_bytes();
    let literal = literal.as_bytes();
    let mut matches = vec![vec![false; literal.len() + 1]; pattern.len() + 1];
    matches[0][0] = true;
    for pattern_index in 0..pattern.len() {
        match pattern[pattern_index] {
            b'*' => {
                for literal_index in 0..=literal.len() {
                    matches[pattern_index + 1][literal_index] |=
                        matches[pattern_index][literal_index];
                    if literal_index < literal.len() {
                        matches[pattern_index + 1][literal_index + 1] |=
                            matches[pattern_index + 1][literal_index];
                    }
                }
            }
            b'?' => {
                for literal_index in 0..literal.len() {
                    matches[pattern_index + 1][literal_index + 1] |=
                        matches[pattern_index][literal_index];
                }
            }
            byte => {
                for literal_index in 0..literal.len() {
                    if byte == literal[literal_index] {
                        matches[pattern_index + 1][literal_index + 1] |=
                            matches[pattern_index][literal_index];
                    }
                }
            }
        }
    }
    matches[pattern.len()][literal.len()]
}

fn path_components(path: &str) -> Vec<String> {
    let mut components = Vec::new();
    for component in path.replace('\\', "/").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value.to_string()),
        }
    }
    components
}

fn components_to_path(components: &[String]) -> String {
    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

fn logical_cwd(command_cwd: Option<&str>, execution_cwd: Option<&Path>) -> Option<String> {
    let cwd = execution_cwd.map(|path| normalized_shell_path(path.to_string_lossy().as_ref()))?;
    let raw = command_cwd.map(str::trim).filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return Some(cwd);
    };
    let normalized = normalized_shell_path(raw);
    if normalized == "." {
        return Some(cwd);
    }
    if !is_absolute_host_path(normalized.as_str()) {
        return Some(components_to_path(
            path_components(format!("{cwd}/{normalized}").as_str()).as_slice(),
        ));
    }
    Some(components_to_path(
        path_components(normalized.as_str()).as_slice(),
    ))
}

fn is_absolute_host_path(path: &str) -> bool {
    path.starts_with('/')
}

fn normalized_shell_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("//?/") {
        normalized = if let Some(unc) = rest.strip_prefix("UNC/") {
            format!("//{unc}")
        } else {
            rest.to_string()
        };
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = normalized[2..].trim_start_matches('/');
        return if rest.is_empty() {
            format!("/{drive}")
        } else {
            format!("/{drive}/{rest}")
        };
    }
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_shell_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 2 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic()
}

fn normalized_node_text(node: Node<'_>, source: &[u8]) -> Result<String, String> {
    let raw = node
        .utf8_text(source)
        .map_err(|error| format!("read Bash syntax node failed: {error}"))?;
    Ok(normalize_shell_word(raw))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    Unquoted,
    Single,
    Double,
}

fn normalize_shell_word(raw: &str) -> String {
    let characters = raw.chars().collect::<Vec<_>>();
    let mut normalized = String::new();
    let mut state = QuoteState::Unquoted;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        match state {
            QuoteState::Unquoted => match character {
                '\'' => state = QuoteState::Single,
                '"' => state = QuoteState::Double,
                '\\' => {
                    index += 1;
                    if let Some(escaped) = characters.get(index).copied() {
                        push_literal_character(&mut normalized, escaped);
                    }
                }
                '$' => {
                    if let Some(consumed) = home_expansion_length(&characters[index..]) {
                        normalized.push(HOME_ROOT_MARKER);
                        index += consumed - 1;
                    } else {
                        normalized.push(DYNAMIC_EXPANSION_MARKER);
                    }
                }
                '~' if normalized.is_empty()
                    && characters.get(index + 1).is_none_or(|next| *next == '/') =>
                {
                    normalized.push(HOME_ROOT_MARKER);
                }
                value => normalized.push(value),
            },
            QuoteState::Single => match character {
                '\'' => state = QuoteState::Unquoted,
                value => push_literal_character(&mut normalized, value),
            },
            QuoteState::Double => match character {
                '"' => state = QuoteState::Unquoted,
                '\\' => {
                    if let Some(escaped) = characters.get(index + 1).copied() {
                        if matches!(escaped, '$' | '`' | '"' | '\\' | '\n') {
                            index += 1;
                            push_literal_character(&mut normalized, escaped);
                        } else {
                            normalized.push('\\');
                        }
                    } else {
                        normalized.push('\\');
                    }
                }
                '$' => {
                    if let Some(consumed) = home_expansion_length(&characters[index..]) {
                        normalized.push(HOME_ROOT_MARKER);
                        index += consumed - 1;
                    } else {
                        normalized.push(DYNAMIC_EXPANSION_MARKER);
                    }
                }
                value => push_literal_character(&mut normalized, value),
            },
        }
        index += 1;
    }
    normalized
}

fn push_literal_character(target: &mut String, character: char) {
    if matches!(character, '*' | '?' | '[') {
        target.push(LITERAL_GLOB_MARKER);
    } else {
        target.push(character);
    }
}

fn home_expansion_length(characters: &[char]) -> Option<usize> {
    if characters.starts_with(&['$', 'H', 'O', 'M', 'E'])
        && characters
            .get(5)
            .is_none_or(|next| !(*next == '_' || next.is_ascii_alphanumeric()))
    {
        return Some(5);
    }
    if !characters.starts_with(&['$', '{', 'H', 'O', 'M', 'E']) {
        return None;
    }
    let closing = characters.iter().position(|character| *character == '}')?;
    let expression = characters[2..closing].iter().collect::<String>();
    (expression == "HOME" || expression.starts_with("HOME:?") || expression.starts_with("HOME?"))
        .then_some(closing + 1)
}

fn executable_basename(executable: &str) -> &str {
    executable.rsplit('/').next().unwrap_or(executable)
}

#[cfg(test)]
mod tests {
    use super::{directly_recursively_deleted_protected_root, ProtectedRoot};
    use std::path::Path;

    fn inspect(command: &str) -> Option<ProtectedRoot> {
        directly_recursively_deleted_protected_root(
            command,
            None,
            Some(Path::new("/host/workspace")),
        )
        .expect("inspect command")
    }

    #[test]
    fn blocks_direct_recursive_rm_of_protected_roots() {
        let cases = [
            ("rm -rf /", ProtectedRoot::System),
            (
                "/bin/rm -fr -- /host/workspace",
                ProtectedRoot::WorkingDirectory,
            ),
            (
                "command rm --recursive /host/workspace/*",
                ProtectedRoot::WorkingDirectory,
            ),
            ("env MODE=test rm -R \"$HOME\"", ProtectedRoot::Home),
            ("rm -r \"${HOME}\"", ProtectedRoot::Home),
            ("rm -rf ~/*", ProtectedRoot::Home),
            ("rm -rf \"$HOME\"/*", ProtectedRoot::Home),
            ("rm -rf \"$HOME/.centaeris\"", ProtectedRoot::HomeMetadata),
            ("rm -rf ~/.centaeris/*", ProtectedRoot::HomeMetadata),
            (
                "echo ready && rm -rf /host/workspace/.*",
                ProtectedRoot::WorkingDirectory,
            ),
            (
                "rm -rf /host/workspace/{*,.*}",
                ProtectedRoot::WorkingDirectory,
            ),
            ("rm -rf .", ProtectedRoot::WorkingDirectory),
            ("rm -rf ./*", ProtectedRoot::WorkingDirectory),
            ("rm -rf /host/work*", ProtectedRoot::WorkingDirectory),
            (
                "value=$(rm -rf /host/workspace)",
                ProtectedRoot::WorkingDirectory,
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(inspect(command), Some(expected), "{command}");
        }
    }

    #[test]
    fn blocks_relative_target_that_resolves_to_cwd() {
        let result = directly_recursively_deleted_protected_root(
            "rm -rf ../*",
            Some("src"),
            Some(Path::new("/host/workspace")),
        )
        .expect("inspect relative command");
        assert_eq!(result, Some(ProtectedRoot::WorkingDirectory));
    }

    #[test]
    fn allows_normal_bash_and_scoped_recursive_deletion() {
        for command in [
            "echo hi",
            "apt-get update",
            "pip install -r requirements.txt",
            "npm install",
            "cargo add serde",
            "rm -rf node_modules",
            "rm -rf build*",
            "rm -rf /host/workspace/build",
            "rm -rf \"$HOME/.cache/some-package\"",
            "rm -f /host/workspace",
            "rm -rf /tmp/test-output",
            "echo 'rm -rf /'",
            "# rm -rf /",
            "command -v rm -rf /",
            "cat <<'EOF'\nrm -rf /\nEOF",
            "cleanup() { rm -rf /host/workspace/*; }",
            "python -c 'import shutil; shutil.rmtree(\"/host/workspace\")'",
        ] {
            assert_eq!(inspect(command), None, "{command}");
        }
    }

    #[test]
    fn single_quoted_home_and_quoted_glob_are_literals() {
        assert_eq!(inspect("rm -rf '$HOME'"), None);
        assert_eq!(inspect("rm -rf /host/workspace/'*'"), None);
        assert_eq!(inspect("rm -rf \"~\""), None);
    }

    #[test]
    fn blocks_windows_drive_roots_and_windows_cwd() {
        let cwd = Path::new(r"D:\Projects\Centaeris");
        for (command, expected) in [
            ("rm -rf C:/", ProtectedRoot::System),
            (
                "rm -rf D:/Projects/Centaeris",
                ProtectedRoot::WorkingDirectory,
            ),
        ] {
            let result = directly_recursively_deleted_protected_root(command, None, Some(cwd))
                .expect("inspect Windows path command");
            assert_eq!(result, Some(expected), "{command}");
        }
    }
}
