pub fn print_hello_world() {
    println!("Hello, world!");
}

pub fn print_help(app_name: &str, version: &str, author: &str, about: &str, help_text: &str) {
    println!("{} {}", app_name, version);
    println!("By: {}", author);
    println!("{}\n", about);
    println!("{}", help_text);
}

pub fn print_version(app_name: &str, version: &str) {
    println!("{} {}", app_name, version);
}

pub fn print_error(error_message: &str) {
    eprintln!("Error: {}", error_message);
}
