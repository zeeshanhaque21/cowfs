use cowfs_treehouse::Env;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cowfs_treehouse::run(Env::default(), args));
}
