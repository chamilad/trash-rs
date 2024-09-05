use std::env;



fn main() {
    // list trash
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args[0].clone();
    match command.as_str() {
        "list" => {
            // get user trash directory
            // iterate through entries in files and read the matching trashinfo, show the filename based on the entry
            // in trashinfo
            // 
            // do the same for every mounted drive
        },
        _ =>  {
            println!("unsupported command: {command}");
            std::process::exit(1);
        }
    }
    
}
