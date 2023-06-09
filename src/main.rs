use std::env::args_os;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::Read;
use std::process::exit;

mod git;

#[tokio::main]
async fn main() {
    let mut args = args_os();
    let cmd_name = args.next().expect("cmd name").into_string().unwrap();
    match args.next().map(|x| x.into_string().unwrap()).as_deref() {
        Some("--help" | "help") => print_help(cmd_name),
        Some("edit") => {
            let file = args.next().expect("no file specified");
            let mut buffer = vec![];
            File::open(file)
                .expect("cannot open file")
                .read_to_end(&mut buffer)
                .expect("reading file");
            let errors = check_commit_message(&buffer);
            if !errors.is_empty() {
                eprintln!("we found errors in commit message:");
                print_errors(errors);
                exit(1);
            }
        }
        Some("check") => {
            let git = git::GitRepository::new_cwd();
            let head_name = args
                .next()
                .expect("no head specified")
                .into_string()
                .unwrap();
            let base_name = args
                .next()
                .expect("no base specified")
                .into_string()
                .unwrap();
            let head = git
                .rev_parse(&head_name)
                .await
                .expect("error calling git")
                .expect("unknown head ref");
            let base = git
                .rev_parse(&base_name)
                .await
                .expect("error calling git")
                .expect("unknown base ref");

            let mut have_err = false;

            for commit_hash in git
                .get_commits(head, base)
                .await
                .expect("get commit list failed")
            {
                let commit = git
                    .get_commit(commit_hash)
                    .await
                    .expect("getting commit")
                    .expect("not found");
                let errors = check_commit_message(&commit.message);
                if !errors.is_empty() {
                    eprintln!("we found errors in commit message of {commit_hash}");
                    print_errors(errors);
                    have_err = true;
                }
            }
            if have_err {
                exit(1);
            }
        }
        Some(cmd) => {
            eprintln!("invalid command: {cmd}");
            exit(1);
        }
        None => {
            print_help(cmd_name);
            exit(1);
        }
    }
}

fn print_errors(errors: Vec<MessageError>) {
    for x in errors {
        eprintln!("  {}", x);
    }
}

fn print_help(cmd_name: String) {
    eprintln!("Usage: {cmd_name} {{COMMAND}} [ARGUMENTS]");
    eprintln!("single binary commitlint only for conventional");
    eprintln!("version {}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("COMMANDS:");
    eprintln!("\thelp|--help: Show this help message");
    eprintln!("\tedit: lint for commit-msg hook");
    eprintln!("\t\tUsage: {cmd_name} edit {{file_path}}");
    eprintln!("\tcheck: lint for ci");
    eprintln!("\t\tUsage: {cmd_name} check {{HEAD_COMMIT}} {{BASE_COMMIT}}");
}

#[test]
fn check_commit_message_test() {
    macro_rules! test {
        ($message: literal$(, $err: ident $(( $($parm: expr),* $(,)? ))? )* $(,)?) => {
            assert_eq!(
                check_commit_message($message),
                vec![
                    $(MessageError::$err $(( $($parm),* ))? ),*
                ]
            );
        };
    }

    test!(b"feat: Test commit");
    test!(b"feat(scope): Test commit");
    test!(b"feat(scope)!: Test commit");
    test!(b"feat(scope)!: Test commit\n\nBREAKING CHANGES: breaking");

    test!(b"\xff", NotUtf8);
    test!(b"Message Only", HeaderNotFormatted);
    test!(b"feat(not closed scope", HeaderNotFormatted);
    test!(b"feat:no space after colon", HeaderNoSpaceAfterColon);
    test!(b"FEAT: test", HeaderTypeNotLower);
    test!(b"tag: Test commit", HeaderUnknownType("tag".to_string()));
    test!(b"fix: Not trimmed ", HeaderSubjectNotTrimmed);
    test!(b"fix:  Not trimmed", HeaderSubjectNotTrimmed);
    // \xE3\x80\x80: U+3000
    test!(b"fix: \xE3\x80\x80Not trimmed", HeaderSubjectNotTrimmed);

    test!(b"fix: I fixed some bug", HeaderSubjectMustNotASentence);
    test!(b"fix: We fixed some bug", HeaderSubjectMustNotASentence);
    test!(b"fix: You cannot use that", HeaderSubjectMustNotASentence);
    test!(b"fix: ", HeaderSubjectEmpty);
    test!(b"fix:", HeaderSubjectEmpty);
    test!(b"feat(scope)!: Test commit\nmessage", NoEmptyLineBeforeBody);
    test!(
        b"feat(scope): Test commit\n\nBREAKING CHANGES: breaking",
        NoBangInBreakingChangeCommit,
    );
}

fn check_commit_message(title: &[u8]) -> Vec<MessageError> {
    let prefixes: &[&[u8]] = &[
        // merge: see fmt_merge_msg_title in fmt-merge-msg.c
        b"Merge branch ",
        b"Merge branches ",
        b"Merge remote-tracking branch ",
        b"Merge remote-tracking branches ",
        b"Merge tag ",
        b"Merge tags ",
        b"Merge commit ",
        b"Merge commits ",
        b"Merge HEAD ",
        // revert
        b"Revert \"",
        // merge pull request
        b"Merge pull request #",
    ];
    for prefix in prefixes {
        if title.starts_with(prefix) {
            return vec![]
        }
    }
    let Ok(title) = std::str::from_utf8(title) else {
        return vec![MessageError::NotUtf8]
    };

    let mut errors = Vec::new();

    let lines = title.lines().collect::<Vec<_>>();

    let is_breaking = check_header(lines[0], &mut errors);

    if lines.len() == 1 {
        return errors;
    }

    if lines[1] != "" {
        errors.push(MessageError::NoEmptyLineBeforeBody);
    }

    let message_lines = &lines[2..];

    fn is_breaking_footer(line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.starts_with("BREAKING CHANGE") || trimmed.starts_with("BREAKING-CHANGE")
    }

    if let Some(is_breaking) = is_breaking {
        let footer_breaking = message_lines.iter().any(|&x| is_breaking_footer(x));
        if footer_breaking && !is_breaking {
            errors.push(MessageError::NoBangInBreakingChangeCommit);
        }
    }

    errors
}

fn check_header(line: &str, errors: &mut Vec<MessageError>) -> Option<bool> {
    fn parse<'a>(line: &'a str) -> Option<(&'a str, Option<&'a str>, bool, &'a str)> {
        let ty_end = line.find(|x| !matches!(x, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-'))?;
        let (ty, mut rest) = line.split_at(ty_end);

        let scope;
        if rest.starts_with("(") {
            rest = &rest[1..];
            let close_scope = rest.find(')')?;
            scope = Some(&rest[..close_scope]);
            rest = &rest[close_scope + 1..];
        } else {
            scope = None;
        }
        let is_breaking;
        if rest.starts_with("!") {
            is_breaking = true;
            rest = &rest[1..];
        } else {
            is_breaking = false;
        }

        if !rest.starts_with(":") {
            return None;
        }
        rest = &rest[1..];

        Some((ty, scope, is_breaking, rest))
    }

    let mut parsed = parse(line);

    if let Some((ty, _, _, ref mut subject)) = parsed {
        let ty_lower = ty.to_ascii_lowercase();
        if ty_lower != ty {
            errors.push(MessageError::HeaderTypeNotLower);
        }
        if !matches!(
            ty_lower.as_str(),
            "build"
                | "chore"
                | "ci"
                | "docs"
                | "feat"
                | "fix"
                | "perf"
                | "refactor"
                | "revert"
                | "style"
                | "test"
        ) {
            errors.push(MessageError::HeaderUnknownType(ty.to_string()));
        }
        if *subject == "" {
            // fix
        } else if subject.starts_with(" ") {
            *subject = &subject[1..];
        } else {
            errors.push(MessageError::HeaderNoSpaceAfterColon);
        }
    } else {
        errors.push(MessageError::HeaderNotFormatted);
    }

    let subject = parsed.map(|x| x.3).unwrap_or(line);

    let trimmed_subject = subject.trim();
    if trimmed_subject != subject {
        errors.push(MessageError::HeaderSubjectNotTrimmed);
    }
    let subject = trimmed_subject.to_ascii_lowercase();
    let mut like_a_sentence = false;
    if subject.ends_with(".") {
        like_a_sentence = true;
    }
    if subject.starts_with("i ") | subject.starts_with("we ") | subject.starts_with("you ") {
        // disallow sentence with person as the subject
        like_a_sentence = true;
    }
    if like_a_sentence {
        errors.push(MessageError::HeaderSubjectMustNotASentence);
    }
    if subject == "" {
        errors.push(MessageError::HeaderSubjectEmpty);
    }

    parsed.map(|x| x.2)
}

#[derive(Debug, Eq, PartialEq)]
enum MessageError {
    NotUtf8,

    // about header line
    HeaderNotFormatted,
    HeaderNoSpaceAfterColon,
    HeaderTypeNotLower,
    HeaderUnknownType(String),
    HeaderSubjectNotTrimmed,
    HeaderSubjectMustNotASentence,
    HeaderSubjectEmpty,
    NoEmptyLineBeforeBody,
    NoBangInBreakingChangeCommit,
}

impl Display for MessageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageError::NotUtf8 => f.write_str("commit message is not utf8"),
            MessageError::HeaderNotFormatted => f.write_str("commit first line is not formatted"),
            MessageError::HeaderNoSpaceAfterColon => f.write_str("no space after ':'"),
            MessageError::HeaderTypeNotLower => f.write_str("commit type is not lowercase"),
            MessageError::HeaderUnknownType(ty) => write!(f, "unknown header type: {}", ty),
            MessageError::HeaderSubjectNotTrimmed => {
                f.write_str("commit subject contains extra spaces")
            }
            MessageError::HeaderSubjectMustNotASentence => {
                f.write_str("commit subject seems like a sentence")
            }
            MessageError::HeaderSubjectEmpty => f.write_str("commit subject is empty"),
            MessageError::NoEmptyLineBeforeBody => {
                f.write_str("there is no empty line before body")
            }
            MessageError::NoBangInBreakingChangeCommit => {
                f.write_str("no '!' in first line in breaking change commit")
            }
        }
    }
}
