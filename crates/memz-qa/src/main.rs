use std::io::Read;
fn main() {
    let result = (|| {
        let args: Vec<_> = std::env::args().skip(1).collect();
        let kind = args.first().ok_or("checker command required")?;
        let value = if args.len() == 1 {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| e.to_string())?;
            serde_json::from_str(&input).map_err(|e| e.to_string())?
        } else if [
            "release-validation",
            "qualify-performance",
            "qualify-quality",
        ]
        .contains(&kind.as_str())
            && args.len() == 5
            && args[1] == "--manifest"
            && args[3] == "--run-id-env"
        {
            serde_json::json!({"repo":std::env::current_dir().map_err(|e|e.to_string())?,"manifest":args[2],"runId":std::env::var(&args[4]).map_err(|_|"run ID environment variable is missing")?})
        } else {
            return Err("unsupported command arguments".into());
        };
        memz_qa::run(kind, &value)
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            if std::env::args().nth(2).as_deref() == Some("--manifest")
                && value["failed"].as_array().is_some_and(|a| !a.is_empty())
            {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("checker unavailable: {error}");
            std::process::exit(2);
        }
    }
}
