pub struct Greeter;

impl Greeter {
    pub fn welcome(&self, name: &str) -> String {
        format_name(name)
    }
}

fn format_name(name: &str) -> String {
    name.trim().to_owned()
}

