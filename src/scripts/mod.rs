//! Scripting engine to run scripts based on tags.
//! This module serves to filter and run the scripts selected by the user.
//!
//! A new commandline and configuration file option was added.
//!
//! --scripts
//!
//!      default
//!          This is the default behavior, like as it was from the beginning of RustScan.
//!          The user do not have to chose anything for this. This is the only script embedded in RustScan running as default.
//!
//!      none
//!          The user have to use the --scripts none commandline argument or scripts = "none" in the config file.
//!          None of the scripts will run, this replaces the removed --no-nmap option.
//!
//!      custom
//!          The user have to use the --scripts custom commandline argument or scripts = "custom" in the config file.
//!          Rustscan will look for the script configuration file in the user's home dir: home_dir/.rustscan_scripts.toml
//!          The config file have 3 optional fields, tag, developer and port. Just the tag field will be used forther in the process.
//!          RustScan will also look for available scripts in the user's home dir: home_dir/.rustscan_scripts
//!          and will try to read all the files, and parse them into a vector of ScriptFiles.
//!          Filtering on tags means the tags found in the rustscan_scripts.toml file will also have to be present in the Scriptfile,
//!          otherwise the script will not be selected.
//!          All of the rustscan_script.toml tags have to be present at minimum in a Scriptfile to get selected, but can be also more.
//!
//! Config file example:
//! fixtures/test_rustscan_scripts.toml
//!
//! Script file examples:
//! fixtures/test_script.py
//! fixtures/test_script.pl
//! fixtures/test_script.sh
//! fixtures/test_script.txt
//!
//! call_format in script files can be of 2 variants.
//! One is where all of the possible tags {{script}} {{ip}} {{port}} are there.
//!     The {{script}} part will be replaced with the scriptfile full path gathered while parsing available scripts.
//!     The {{ip}} part will be replaced with the ip we got from the scan.
//!     The {{port}} part will be reaplced with the ports separated with the ports_separator found in the script file
//!
//! And when there is only {{ip}} and {{port}} is in the format, ony those will be replaced with the arguments from the scan.
//! This makes it easy to run a system installed command like nmap, and give any kind of arguments to it.
//!
//! If the format is different, the script will be silently discarded and will not run. With the Debug option it's possible to see where it goes wrong.

use crate::input::ScriptsRequired;
use anyhow::{anyhow, Result};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, prelude::*};
use std::net::IpAddr;
use std::path::PathBuf;
use subprocess::{Exec, ExitStatus};
use text_placeholder::Template;

static DEFAULT: &'static str = r#"tags = ["core_approved", "RustScan", "default"]
developer = [ "RustScan", "https://github.com/RustScan" ]
ports_separator = ","
call_format = "nmap -vvv -p {{port}} {{ip}}"
"#;

pub fn init_scripts(scripts: ScriptsRequired) -> Result<Vec<ScriptFile>> {
    let mut scripts_to_run: Vec<ScriptFile> = Vec::new();

    match scripts {
        ScriptsRequired::None => Ok(scripts_to_run),
        ScriptsRequired::Default => {
            let default_script =
                toml::from_str::<ScriptFile>(&DEFAULT).expect("Failed to parse Script file.");
            scripts_to_run.push(default_script);
            Ok(scripts_to_run)
        }
        ScriptsRequired::Custom => {
            let scripts_dir_base = match dirs::home_dir() {
                Some(dir) => dir,
                None => return Err(anyhow!("Could not infer scripts path.")),
            };
            let script_paths = match find_scripts(scripts_dir_base) {
                Ok(script_paths) => script_paths,
                Err(e) => return Err(anyhow!(e)),
            };
            debug!("Scripts paths \n{:?}", script_paths);

            let parsed_scripts = parse_scripts(script_paths);
            debug!("Scripts parsed \n{:?}", parsed_scripts);

            let script_config = match ScriptConfig::read_config() {
                Ok(script_config) => script_config,
                Err(e) => return Err(anyhow!(e)),
            };
            debug!("Script config \n{:?}", script_config);

            // Only Scripts that contain all the tags found in ScriptConfig will be selected.
            if script_config.tags.is_some() {
                let config_hashset: HashSet<String> =
                    script_config.tags.unwrap().into_iter().collect();
                for script in &parsed_scripts {
                    if script.tags.is_some() {
                        let script_hashset: HashSet<String> =
                            script.tags.clone().unwrap().into_iter().collect();
                        if config_hashset.is_subset(&script_hashset) {
                            scripts_to_run.push(script.to_owned());
                        } else {
                            debug!(
                                "\nScript tags does not match config tags {:?} {}",
                                &script_hashset,
                                script.path.clone().unwrap().display()
                            );
                        }
                    }
                }
            }
            debug!("\nScript(s) to run {:?}", scripts_to_run);
            Ok(scripts_to_run)
        }
    }
}

pub fn parse_scripts(scripts: Vec<PathBuf>) -> Vec<ScriptFile> {
    let mut parsed_scripts: Vec<ScriptFile> = Vec::with_capacity(scripts.len());
    for script in scripts {
        debug!("Parsing script {}", &script.display());
        if let Some(script_file) = ScriptFile::new(script) {
            parsed_scripts.push(script_file);
        }
    }
    parsed_scripts
}

#[derive(Clone, Debug)]
pub struct Script {
    // Path to the script itself.
    path: Option<PathBuf>,

    // Ip got from scanner.
    ip: IpAddr,

    // Ports found with portscan.
    open_ports: Vec<u16>,

    // Port found in ScriptFile, if defined only this will run with the ip.
    trigger_port: Option<String>,

    // Character to join ports in case we want to use a string format of them, for example nmap -p.
    ports_separator: Option<String>,

    // Tags found in ScriptFile.
    tags: Option<Vec<String>>,

    // The format how we want the script to run.
    call_format: Option<String>,
}

#[derive(Serialize)]
struct ExecPartsScript {
    script: String,
    ip: String,
    port: String,
}

#[derive(Serialize)]
struct ExecParts {
    ip: String,
    port: String,
}

impl Script {
    pub fn build(
        path: Option<PathBuf>,
        ip: IpAddr,
        open_ports: Vec<u16>,
        trigger_port: Option<String>,
        ports_separator: Option<String>,
        tags: Option<Vec<String>>,
        call_format: Option<String>,
    ) -> Self {
        Self {
            path: path,
            ip: ip,
            open_ports: open_ports,
            trigger_port: trigger_port,
            ports_separator: ports_separator,
            tags: tags,
            call_format: call_format,
        }
    }

    // Some variables get changed before read, and compiler throws warning on warn(unused_assignments)
    #[allow(unused_assignments)]
    pub fn run(self) -> Result<String> {
        debug!("run self {:?}", &self);

        let separator = self.ports_separator.unwrap_or(",".into());

        let mut ports_str = self
            .open_ports
            .iter()
            .map(|port| port.to_string())
            .collect::<Vec<String>>()
            .join(&separator);
        if let Some(port) = self.trigger_port {
            ports_str = port;
        }

        let mut final_call_format = String::new();
        if let Some(call_format) = self.call_format {
            final_call_format = call_format;
        } else {
            return Err(anyhow!("Failed to parse execution format."));
        }
        let default_template: Template = Template::new(&final_call_format);
        let mut to_run = String::new();

        if final_call_format.contains("{{script}}") {
            let exec_parts_script: ExecPartsScript = ExecPartsScript {
                script: self.path.unwrap().to_str().unwrap().to_string(),
                ip: self.ip.to_string(),
                port: ports_str,
            };
            to_run = default_template.fill_with_struct(&exec_parts_script)?;
        } else {
            let exec_parts: ExecParts = ExecParts {
                ip: self.ip.to_string(),
                port: ports_str,
            };
            to_run = default_template.fill_with_struct(&exec_parts)?;
        }

        debug!("\nTo run {}", to_run);

        let arguments = shell_words::split(
            &to_run
                .split(" ")
                .map(|arg| arg.to_string())
                .collect::<Vec<String>>()
                .join(" "),
        )
        .expect("Failed to parse script arguments");

        match execute_script(arguments) {
            Ok(result) => return Ok(result),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(not(tarpaulin_include))]
fn execute_script(mut arguments: Vec<String>) -> Result<String> {
    debug!("\nArguments vec: {:?}", &arguments);
    let process = Exec::cmd(&arguments.remove(0)).args(&arguments);
    match process.capture() {
        Ok(c) => {
            let es = match c.exit_status {
                ExitStatus::Exited(c) => c as i32,
                ExitStatus::Signaled(c) => c as i32,
                ExitStatus::Other(c) => c,
                _ => -1,
            };
            if es != 0 {
                return Err(anyhow!("Exit code = {}", es));
            }
            Ok(c.stdout_str())
        }
        Err(error) => {
            debug!("Command error {}", error.to_string());
            return Err(anyhow!(error.to_string()));
        }
    }
}

pub fn find_scripts(mut path: PathBuf) -> Result<Vec<PathBuf>> {
    path.push(".rustscan_scripts");
    if path.is_dir() {
        debug!("Scripts folder found {}", &path.display());
        let mut files_vec: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            files_vec.push(entry.path());
        }
        return Ok(files_vec);
    } else {
        return Err(anyhow!("Can't find scripts folder"));
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScriptFile {
    pub path: Option<PathBuf>,
    pub tags: Option<Vec<String>>,
    pub developer: Option<Vec<String>>,
    pub port: Option<String>,
    pub ports_separator: Option<String>,
    pub call_format: Option<String>,
}

impl ScriptFile {
    fn new(script: PathBuf) -> Option<ScriptFile> {
        let real_path = script.clone();
        let mut lines_buf = String::new();
        if let Ok(file) = File::open(script) {
            for line in io::BufReader::new(file).lines().skip(1) {
                if let Ok(mut line) = line {
                    if line.starts_with("#") {
                        line.retain(|c| c != '#');
                        line = line.trim().to_string();
                        line.push_str("\n");
                        lines_buf.push_str(&line);
                    } else {
                        break;
                    }
                }
            }
        } else {
            debug!("Failed to read file: {}", &real_path.display());
            return None;
        }
        debug!("ScriptFile {} lines\n{}", &real_path.display(), &lines_buf);

        match toml::from_str::<ScriptFile>(&lines_buf) {
            Ok(mut parsed) => {
                debug!("Parsed ScriptFile{} \n{:?}", &real_path.display(), &parsed);
                parsed.path = Some(real_path);
                // parsed_scripts.push(parsed);
                return Some(parsed);
            }
            Err(e) => {
                debug!("Failed to parse ScriptFile headers {}", e.to_string());
                return None;
            }
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ScriptConfig {
    pub tags: Option<Vec<String>>,
    pub ports: Option<Vec<String>>,
    pub developer: Option<Vec<String>>,
}

#[cfg(not(tarpaulin_include))]
impl ScriptConfig {
    pub fn read_config() -> Result<ScriptConfig> {
        let mut home_dir = match dirs::home_dir() {
            Some(dir) => dir,
            None => return Err(anyhow!("Could not infer ScriptConfig path.")),
        };
        home_dir.push(".rustscan_scripts.toml");

        let content = fs::read_to_string(home_dir)?;
        let config = toml::from_str::<ScriptConfig>(&content)?;
        Ok(config)
    }
}