mod db;
mod disassemble;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use indicatif::ProgressBar;
use itertools::Itertools;
use patricia_tree::StringPatriciaMap;
use patternsleuth::resolvers::resolve_self;
use patternsleuth::Image;

use patternsleuth::scanner::Xref;
use patternsleuth::{
    patterns::{get_patterns, Sig},
    scanner::Pattern,
    PatternConfig, Resolution, ResolutionType,
};

#[derive(Parser)]
enum Commands {
    Scan(CommandScan),
    Symbols(CommandSymbols),
    BuildIndex(CommandBuildIndex),
    ViewSymbol(CommandViewSymbol),
    AutoGen(CommandAutoGen),
}

fn parse_maybe_hex(s: &str) -> Result<usize> {
    Ok(s.strip_prefix("0x")
        .map(|s| usize::from_str_radix(s, 16))
        .unwrap_or_else(|| s.parse())?)
}

#[derive(Parser)]
struct CommandScan {
    /// A game to scan (can be specified multiple times). Scans everything if omitted. Supports
    /// globs
    #[arg(short, long)]
    game: Vec<String>,

    /// A game process ID to attach to and scan
    #[arg(long)]
    pid: Option<i32>,

    /// A signature to scan for (can be specified multiple times). Scans for all signatures if omitted
    #[arg(short, long)]
    signature: Vec<Sig>,

    /// Show disassembly context for each stage of every match (I recommend only using with
    /// aggressive filters)
    #[arg(short, long)]
    disassemble: bool,

    /// Show disassembly context for each matched address
    #[arg(long)]
    disassemble_merged: bool,

    /// A pattern to scan for (can be specified multiple times)
    #[arg(short, long, value_parser(|s: &_| Pattern::new(s)))]
    patterns: Vec<Pattern>,

    /// A path to a JSON pattern config file
    #[arg(long)]
    pattern_config: Option<PathBuf>,

    /// An xref to scan for (can be specified multiple times)
    #[arg(short, long, value_parser(|s: &str| parse_maybe_hex(s).map(Xref)))]
    xref: Vec<Xref>,

    /// Load and display symbols from PDBs when available (can be slow)
    #[arg(long)]
    symbols: bool,

    /// Skip parsing of exception table
    #[arg(long)]
    skip_exceptions: bool,

    /// Show scan summary
    #[arg(long)]
    summary: bool,

    /// Show scan progress
    #[arg(long)]
    progress: bool,
}

#[derive(Parser)]
struct CommandSymbols {
    /// A game to scan (can be specified multiple times). Scans everything if omitted. Supports
    /// globs
    #[arg(short, long)]
    game: Vec<String>,

    #[arg(short, long)]
    symbol: Vec<regex::Regex>,
}

#[derive(Parser)]
struct CommandBuildIndex {
    /// A game to scan (can be specified multiple times). Scans everything if omitted. Supports
    /// globs
    #[arg(short, long)]
    game: Vec<String>,
}

#[derive(Parser)]
struct CommandReadIndex {}

#[derive(Parser)]
struct CommandSearchIndex {
    #[arg()]
    symbol: String,
}

#[derive(Parser)]
struct CommandViewSymbol {
    #[arg()]
    symbol: String,

    /// Whether to show symbols in function disassembly
    #[arg(long)]
    show_symbols: bool,
}

#[derive(Parser)]
struct CommandAutoGen {}

fn find_ext<P: AsRef<Path>>(dir: P, ext: &str) -> Result<Option<PathBuf>> {
    for f in fs::read_dir(dir)? {
        let f = f?.path();
        if f.is_file() && f.extension().and_then(std::ffi::OsStr::to_str) == Some(ext) {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

fn main() -> Result<()> {
    match Commands::parse() {
        Commands::Scan(command) => scan(command),
        Commands::Symbols(command) => symbols(command),
        Commands::BuildIndex(command) => db::build(command),
        Commands::ViewSymbol(command) => db::view(command),
        Commands::AutoGen(command) => db::auto_gen(command),
    }
}

fn scan(command: CommandScan) -> Result<()> {
    let sig_filter = command.signature.into_iter().collect::<HashSet<_>>();
    let include_default = command.patterns.is_empty() && command.xref.is_empty();
    let patterns = get_patterns()?
        .into_iter()
        .filter(|p| {
            sig_filter
                .is_empty()
                .then_some(include_default)
                .unwrap_or_else(|| sig_filter.contains(&p.sig))
        })
        .chain(
            command
                .patterns
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    PatternConfig::new(
                        Sig::Custom("arg".to_string()),
                        format!("pattern {i}"),
                        None,
                        p,
                        resolve_self,
                    )
                })
                .chain(command.xref.into_iter().enumerate().map(|(i, p)| {
                    PatternConfig::xref(
                        Sig::Custom("arg".to_string()),
                        format!("xref {i}"),
                        None,
                        p,
                        resolve_self,
                    )
                })),
        )
        .chain(command.pattern_config.into_iter().flat_map(|path| {
            let file = std::fs::read_to_string(path).unwrap();
            let config: HashMap<String, Vec<String>> = serde_json::from_str(&file).unwrap();

            config.into_iter().flat_map(|(symbol, patterns)| {
                patterns.into_iter().enumerate().map(move |(i, p)| {
                    PatternConfig::new(
                        Sig::Custom(format!("file {symbol}")),
                        format!("#{i} {symbol}"),
                        None,
                        Pattern::new(p).unwrap(),
                        resolve_self,
                    )
                })
            })
        }))
        .collect_vec();

    let sigs = patterns
        .iter()
        .map(|p| p.sig.clone())
        .collect::<HashSet<_>>();

    let mut games: HashSet<String> = Default::default();

    let mut all: HashMap<(String, (&Sig, &String)), Vec<Resolution>> = HashMap::new();

    use colored::Colorize;
    use indicatif::ProgressIterator;
    use itertools::join;
    use prettytable::{format, row, Cell, Row, Table};

    enum Output {
        Stdout,
        Progress(ProgressBar),
    }

    impl Output {
        fn println<M: AsRef<str>>(&self, msg: M) {
            match self {
                Output::Stdout => println!("{}", msg.as_ref()),
                Output::Progress(progress) => progress.println(msg),
            }
        }
    }

    let mut games_vec = vec![];

    if let Some(pid) = command.pid {
        games_vec.push(GameEntry::Process(GameProcessEntry { pid }));
    } else {
        games_vec.extend(get_games(command.game)?.into_iter().map(GameEntry::File));
    }

    let (output, iter): (_, Box<dyn Iterator<Item = _>>) = if command.progress {
        let progress = ProgressBar::new(games_vec.len() as u64);
        (
            Output::Progress(progress.clone()),
            Box::new(games_vec.iter().progress_with(progress)),
        )
    } else {
        (Output::Stdout, Box::new(games_vec.iter()))
    };

    for game in iter {
        #[allow(unused_assignments)]
        let mut bin_data = None;

        let (name, exe) = match game {
            GameEntry::File(GameFileEntry { name, exe_path }) => {
                output.println(format!("{:?} {:?}", name, exe_path.display()));

                bin_data = Some(fs::read(exe_path)?);

                (Cow::Borrowed(name), {
                    let mut builder = Image::builder().functions(!command.skip_exceptions);
                    if command.symbols {
                        builder = builder.symbols(exe_path);
                    }
                    match builder.build(bin_data.as_ref().unwrap()) {
                        Ok(exe) => exe,
                        Err(err) => {
                            output.println(format!("err reading {}: {}", exe_path.display(), err));
                            continue;
                        }
                    }
                })
            }
            GameEntry::Process(GameProcessEntry { pid }) => {
                output.println(format!("PID={pid}"));

                (
                    Cow::Owned(format!("PID={pid}")),
                    patternsleuth::process::external::read_image_from_pid(*pid)?,
                )
            }
        };

        games.insert(name.to_string());

        let scan = exe.scan(&patterns)?;

        // group results by Sig
        let folded_scans = scan
            .results
            .iter()
            .map(|(config, m)| (&config.sig, (config, m)))
            .fold(HashMap::new(), |mut map: HashMap<_, Vec<_>>, (k, v)| {
                map.entry(k).or_default().push(v);
                map
            });

        let mut table = Table::new();
        table.set_titles(row!["sig", "offline scan"]);

        for sig in &sigs {
            let mut cells = vec![];
            cells.push(Cell::new(&sig.to_string()));

            if let Some(sig_scans) = folded_scans.get(&sig) {
                if command.disassemble {
                    let mut table = Table::new();
                    table.set_format(*format::consts::FORMAT_NO_BORDER);
                    for m in sig_scans.iter() {
                        let mut cells = vec![];
                        match &m.1.res {
                            ResolutionType::Address(address) => {
                                cells.push(Cell::new(&format!(
                                    "{}\n{}",
                                    m.0.name,
                                    disassemble::disassemble(
                                        &exe,
                                        *address,
                                        m.1.stages
                                            .is_empty()
                                            .then_some(m.0.scan.scan_type.get_pattern())
                                            .flatten()
                                    )
                                )));
                            }
                            ResolutionType::String(string) => {
                                cells.push(Cell::new(&format!("{:?}\n{:?}", m.0.name, string)));
                            }
                            ResolutionType::Count => {
                                #[allow(clippy::unnecessary_to_owned)]
                                cells.push(Cell::new(&format!("{}\ncount", m.0.name)));
                            }
                            ResolutionType::Failed => {
                                #[allow(clippy::unnecessary_to_owned)]
                                cells.push(Cell::new(&format!("{}\n{}", m.0.name, "failed".red())));
                            }
                        }
                        for (i, stage) in m.1.stages.iter().enumerate().rev() {
                            cells.push(Cell::new(&format!(
                                "stage[{}]\n{}",
                                i,
                                disassemble::disassemble(
                                    &exe,
                                    *stage,
                                    (i == 0)
                                        .then_some(m.0.scan.scan_type.get_pattern())
                                        .flatten()
                                )
                            )));
                        }
                        table.add_row(Row::new(cells));
                    }
                    cells.push(Cell::new(&table.to_string()));
                } else if command.disassemble_merged {
                    cells.push(Cell::new({
                        let cells = sig_scans
                            .iter()
                            .fold(
                                HashMap::<&ResolutionType, HashMap<&str, usize>>::new(),
                                |mut map, m| {
                                    *map.entry(&m.1.res)
                                        .or_default()
                                        .entry(&m.0.name)
                                        .or_default() += 1;
                                    map
                                },
                            )
                            .iter()
                            // sort by pattern name, then match address
                            .sorted_by_key(|&data| data.0)
                            .map(|(m, counts)| match &m {
                                ResolutionType::Address(address) => {
                                    let dis = disassemble::disassemble(&exe, *address, None);

                                    let mut lines = vec![];
                                    for (name, count) in counts.iter().sorted_by_key(|e| e.0) {
                                        let count = if *count > 1 {
                                            format!(" (x{count})")
                                        } else {
                                            "".to_string()
                                        };

                                        lines.push(
                                            format!("{:?}{}", name, count).normal().to_string(),
                                        );
                                    }
                                    lines.push(dis);

                                    Cell::new(&join(lines, "\n"))
                                }
                                _ => todo!(),
                            })
                            .collect::<Vec<_>>();

                        let mut table = Table::new();
                        table.set_format(*format::consts::FORMAT_NO_BORDER);

                        table.add_row(Row::new(cells));

                        &table.to_string()
                    }));
                } else {
                    cells.push(Cell::new({
                        let mut lines = sig_scans
                            .iter()
                            // group and count matches by (pattern name, address)
                            .fold(
                                HashMap::<(&String, &ResolutionType), usize>::new(),
                                |mut map, m| {
                                    *map.entry((&m.0.name, &m.1.res)).or_default() += 1;
                                    map
                                },
                            )
                            .iter()
                            // sort by pattern name, then match address
                            .sorted_by_key(|&data| data.0)
                            .map(|(m, count)| {
                                // add count indicator if more than 1
                                let count = if *count > 1 {
                                    format!(" (x{count})")
                                } else {
                                    "".to_string()
                                };

                                match &m.1 {
                                    ResolutionType::Address(address) => (
                                        format!("{:016x} {:?}{}", address, m.0, count)
                                            .normal()
                                            .to_string(),
                                        exe.symbols
                                            .as_ref()
                                            .and_then(|symbols| symbols.get(address)),
                                    ),
                                    ResolutionType::String(string) => (
                                        format!("{:?} {:?}{}", string, m.0, count)
                                            .normal()
                                            .to_string(),
                                        None,
                                    ),

                                    ResolutionType::Count => (
                                        format!("count {:?}{}", m.0, count).normal().to_string(),
                                        None,
                                    ),
                                    ResolutionType::Failed => (
                                        format!("failed {:?}{}", m.0, count).red().to_string(),
                                        None,
                                    ),
                                }
                            })
                            .collect::<Vec<_>>();
                        let max_len = lines.iter().map(|(line, _)| line.len()).max();
                        for (line, symbol) in &mut lines {
                            if let Some(symbol) = symbol {
                                line.push_str(&format!(
                                    "{}{}",
                                    " ".repeat(1 + max_len.unwrap() - line.len()),
                                    symbol.bright_yellow()
                                ));
                            }
                        }
                        &join(lines.iter().map(|(line, _)| line), "\n").to_string()
                    }));
                }
            } else {
                #[allow(clippy::unnecessary_to_owned)]
                cells.push(Cell::new(&"not found".red().to_string()));
            }

            table.add_row(Row::new(cells));
        }
        output.println(table.to_string());

        // fold current game scans into summary scans
        scan.results.into_iter().fold(&mut all, |map, m| {
            map.entry((name.to_string(), (&m.0.sig, &m.0.name)))
                .or_default()
                .push(m.1);
            map
        });
    }

    // force any progress output to be dropped
    let output = Output::Stdout;

    if command.summary {
        #[derive(Debug, Default)]
        struct Summary {
            matches: usize,
            resolved: usize,
            failed: usize,
        }
        impl Summary {
            fn format(&self) -> String {
                if self.matches == 0 && self.failed == 0 && self.resolved == 0 {
                    "none".to_owned()
                } else {
                    format!("M={} R={} F={}", self.matches, self.resolved, self.failed)
                }
            }
        }

        let mut summary = Table::new();
        let title_strs: Vec<String> = ["".into(), "unqiue addresses".into()]
            .into_iter()
            .chain(
                patterns
                    .iter()
                    .map(|conf| format!("{:?}({})", conf.sig, conf.name)),
            )
            .collect();
        summary.set_titles(Row::new(title_strs.iter().map(|s| Cell::new(s)).collect()));
        let mut totals = patterns.iter().map(|_| Summary::default()).collect_vec();

        let mut no_matches = 0;
        let mut one_match = 0;
        let mut gt_one_match = 0;

        for game in games.iter().sorted() {
            let mut row = vec![Cell::new(game)];

            let mut matched_addresses = HashSet::new();

            let summaries: Vec<Summary> = patterns
                .iter()
                .map(|conf| {
                    let res = all.get(&(game.to_string(), (&conf.sig, &conf.name)));
                    if let Some(res) = res {
                        for res in res {
                            if let ResolutionType::Address(addr) = res.res {
                                matched_addresses.insert(addr);
                            }
                        }
                        Summary {
                            matches: res.len(),
                            resolved: res
                                .iter()
                                .filter(|res| !matches!(res.res, ResolutionType::Failed))
                                .count(),
                            failed: res
                                .iter()
                                .filter(|res| matches!(res.res, ResolutionType::Failed))
                                .count(),
                        }
                    } else {
                        Summary {
                            matches: 0,
                            resolved: 0,
                            failed: 0,
                        }
                    }
                })
                .collect();

            for (i, s) in summaries.iter().enumerate() {
                if s.matches > 0 {
                    totals[i].matches += 1;
                }
                if s.resolved > 0 {
                    totals[i].resolved += 1;
                }
                if s.failed > 0 {
                    totals[i].failed += 1;
                }
            }

            match matched_addresses.len() {
                0 => {
                    no_matches += 1;
                }
                1 => {
                    one_match += 1;
                }
                _ => {
                    gt_one_match += 1;
                }
            }

            row.push(Cell::new(&format!("unique={}", matched_addresses.len())));

            let cell_strs: Vec<String> = summaries.iter().map(Summary::format).collect();
            row.extend(cell_strs.iter().map(|s| Cell::new(s)));
            summary.add_row(Row::new(row));
        }

        let total_strs = [
            format!("total={}", games.len()),
            format!("0={} 1={} >1={}", no_matches, one_match, gt_one_match),
        ]
        .into_iter()
        .chain(totals.iter().map(Summary::format))
        .collect_vec();
        summary.add_row(Row::new(
            total_strs.iter().map(|s| Cell::new(s)).collect_vec(),
        ));

        //let games: HashSet<String> = all.keys().map(|(game, _)| game).cloned().collect();
        //println!("{:#?}", all);

        output.println(summary.to_string());
    }

    Ok(())
}

fn symbols(command: CommandSymbols) -> Result<()> {
    let re = &command.symbol;
    let filter = |name: &_| re.iter().any(|re| re.is_match(name));

    use prettytable::{Cell, Row, Table};

    let mut cells = vec![];

    for GameFileEntry { name, exe_path } in get_games(command.game)? {
        if !exe_path.with_extension("pdb").exists() {
            continue;
        }

        println!("{:?} {:?}", name, exe_path.display());
        let bin_data = fs::read(&exe_path)?;
        let exe = match Image::builder()
            .functions(true)
            .symbols(&exe_path)
            .build(&bin_data)
        {
            Ok(exe) => exe,
            Err(err) => {
                println!("err reading {}: {}", exe_path.display(), err);
                continue;
            }
        };

        for (address, name) in exe.symbols.as_ref().unwrap() {
            if filter(name) {
                if let Some(exception) = exe.get_root_function(*address) {
                    let fns = exe.get_child_functions(exception.range.start);
                    let min = fns.iter().map(|f| f.range.start).min().unwrap();
                    let max = fns.iter().map(|f| f.range.end).max().unwrap();
                    let full_range = min..max; // TODO does not handle sparse ranges
                    if exception.range.start != *address {
                        println!("MISALIGNED EXCEPTION ENTRY FOR {}", name);
                    } else {
                        cells.push((
                            name.clone(),
                            disassemble::disassemble_range(&exe, full_range),
                        ));
                    }
                } else {
                    println!("{:016x} [NO EXCEPT] {}", address, name);
                }
            }
        }
    }

    let mut table = Table::new();
    table.set_titles(cells.iter().map(|c| c.0.clone()).collect());
    table.add_row(Row::new(
        cells.into_iter().map(|c| Cell::new(&c.1)).collect(),
    ));
    table.printstd();

    Ok(())
}

enum GameEntry {
    File(GameFileEntry),
    Process(GameProcessEntry),
}

struct GameFileEntry {
    name: String,
    exe_path: PathBuf,
}

struct GameProcessEntry {
    pid: i32,
}

fn get_games(filter: impl AsRef<[String]>) -> Result<Vec<GameFileEntry>> {
    let games_filter = filter
        .as_ref()
        .iter()
        .map(|g| {
            Ok(globset::GlobBuilder::new(g)
                .case_insensitive(true)
                .build()?
                .compile_matcher())
        })
        .collect::<Result<Vec<_>>>()?;

    fs::read_dir("games")?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .map(|entry| -> Result<Option<(String, PathBuf)>> {
            let dir_name = entry.file_name();
            let name = dir_name.to_string_lossy().to_string();
            if !games_filter
                .is_empty()
                .then_some(true)
                .unwrap_or_else(|| games_filter.iter().any(|g| g.is_match(&name)))
            {
                return Ok(None);
            }

            let Some(exe_path) = find_ext(entry.path(), "exe")
                .transpose()
                .or_else(|| find_ext(entry.path(), "elf").transpose())
                .transpose()?
            else {
                return Ok(None);
            };
            Ok(Some((name, exe_path)))
        })
        .filter_map(|r| r.transpose())
        .collect::<Result<Vec<(String, _)>>>()
        .map(|entries| {
            sample_order(entries, 3)
                .into_iter()
                .map(|(name, exe_path)| GameFileEntry { name, exe_path })
                .collect::<Vec<GameFileEntry>>()
        })
}

/// Distribute pairs such that unique prefixes are encountered early
/// e.g.
/// 7_a 8_a 9_a 7_b 7_c 7_d 8_b 8_c 9_b
fn sample_order<V>(entries: Vec<(String, V)>, prefix_size: usize) -> Vec<(String, V)> {
    let mut trie = StringPatriciaMap::from_iter(entries);
    let mut len = 1;
    let mut result = vec![];
    while !trie.is_empty() {
        let mut prefixes = HashSet::new();
        for (k, _v) in trie.iter() {
            if k.chars().count() >= len {
                prefixes.insert(k.chars().take(len).collect::<String>());
            }
        }
        for p in prefixes.iter().sorted() {
            let take = trie
                .iter_prefix(p)
                .take(prefix_size)
                .map(|(k, _v)| k)
                .collect_vec();
            for k in take {
                let v = trie.remove(k.clone()).unwrap();
                result.push((k, v));
            }
        }
        len += 1;
    }
    result
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_sample_cont() {
        let entries = ["aa", "ba", "ca", "ab", "ac", "bc"]
            .iter()
            .map(|k| (k.to_string(), ()))
            .collect_vec();
        let ordered = sample_order(entries.clone(), 1);
        assert_eq!(entries, ordered);
    }
}
