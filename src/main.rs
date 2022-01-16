#![feature(try_trait_v2)]
use std::error::Error;
use structopt::StructOpt;
use tracc::ast::Program;

use tracc::error::SourceMetadata;
use tracc::grammar::Parser;

// TODO(#3): structured formatting lib (error,warning,note,help, etc)
// TODO(#4): create test crate

fn main() {
    if let Err(ref e) = run() {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    use std::fs;

    let opt = Opt::from_args();
    let filename = opt.file;
    let file = fs::read_to_string(&filename)?;
    let _out_file = opt.output.unwrap_or_else(|| filename.with_extension("s"));
    let meta = SourceMetadata::new(&file).with_file(filename);
    let program: Program = Parser::new(&meta).parse()?;
    let (name, ir) = tracc::intermediate::generate::compile_program(program, &meta)?;
    println!("func {}:", name);
    for (i, block) in ir.into_iter().enumerate() {
        println!("BB{}:", i);
        for stmt in block.statements {
            println!("  {}", stmt);
        }
        println!("  {}", block.end);
    }
    // let program = tracc::variables::convert_program(program, &meta)?;
    // let output = program.compile();
    // let mut file = fs::File::create(out_file)?;
    // for x in output {
    //     writeln!(file, "{}", x)?;
    // }

    Ok(())
}

#[derive(Debug, StructOpt)]
struct Opt {
    /// The file to compile
    #[structopt(parse(from_os_str))]
    file: std::path::PathBuf,
    /// The (optional) output file
    #[structopt(short = "o", long = "output", parse(from_os_str))]
    output: Option<std::path::PathBuf>,
}
