pub fn handle_status(args: &[String]) {
    if args.iter().any(|a| a == "--json") {
        println!("{{}}");
    } else {
        println!("No uncommitted attributions.");
    }
}
