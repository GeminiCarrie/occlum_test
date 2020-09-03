extern crate futures;
extern crate grpc;
extern crate grpc_protobuf;
extern crate protobuf;
#[macro_use]
extern crate log;

pub mod exec;

extern crate env_logger;

use exec::{
    ExecCommRequest, ExecCommResponse_ExecutionStatus, GetResultRequest,
    GetResultResponse_ExecutionStatus, HealthCheckRequest, HealthCheckResponse_ServingStatus,
    KillProcessRequest, OcclumExecClient, StopRequest,
};
use futures::executor;
use grpc::prelude::*;
use grpc::ClientConf;
pub const DEFAULT_SERVER_FILE: &'static str = "occlum_exec_server";
pub const DEFAULT_CLIENT_FILE: &'static str = "occlum_client";
pub const DEFAULT_SOCK_FILE: &'static str = "localhost:7878";
pub const DEFAULT_SERVER_TIMER: u32 = 3;

use protobuf::RepeatedField;
use std::cmp;
use std::env;

use std::path::Path;
use std::process;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{thread, time};
use tempdir::TempDir;

/// Execute the command on server
///
/// # Examples
///
/// use occlum_exec::occlum_exec_grpc::OcclumExecClient;
///
/// let client = OcclumExecClient::new_plain_unix(&sock_file, ClientConf::new()).unwrap();
/// let let occlum_exec: Vec<String> = vec!["/bin/hello_world".to_String(), "".to_String()];
/// let process_id = exec_command(&client, &occlum_exec[0], &occlum_exec[1..]);
///
fn exec_command(
    client: &OcclumExecClient,
    command: &str,
    parameters: &[&str],
    envs: &[&str],
) -> Result<i32, String> {
    debug!("exec_command {:?} {:?} {:?}", command, parameters, envs);

    let mut parameter_list = RepeatedField::default();
    for p in parameters {
        parameter_list.push(p.to_string());
    }

    let mut enviroments_list = RepeatedField::default();
    for env in envs {
        enviroments_list.push(env.to_string());
    }

    let tmp_dir = TempDir::new("occlum_tmp").expect("create temp dir");

    let sockpath = tmp_dir.path().join("remote_client_occlum.sock");

    let resp = executor::block_on(
        client
            .exec_command(
                grpc::RequestOptions::new(),
                ExecCommRequest {
                    process_id: process::id(),
                    command: command.to_string(),
                    parameters: parameter_list,
                    enviroments: enviroments_list,
                    sockpath: String::from(sockpath.as_path().to_str().unwrap()),
                    ..Default::default()
                },
            )
            .drop_metadata(),
    ); // Drop response metadata

    match resp {
        Ok(resp) => match resp.status {
            ExecCommResponse_ExecutionStatus::LAUNCH_FAILED => {
                Err(String::from("failed to launch the process."))
            }
            ExecCommResponse_ExecutionStatus::RUNNING => {
                println!("before sendfd_thread.join().unwrap()----------");
                // sendfd_thread.join().unwrap();
                println!("end sendfd_thread.join().unwrap()-----------");
                Ok(resp.process_id)
            }
        },
        Err(_) => Err(String::from("failed to send request.")),
    }
}

/// Starts the server if the server is not running
fn start_server(client: &OcclumExecClient, server_name: &str) -> Result<u32, String> {
    let mut server_launched = false;
    println!("start_server-------");
    loop {
        let resp = executor::block_on(
            client
                .status_check(
                    grpc::RequestOptions::new(),
                    HealthCheckRequest {
                        ..Default::default()
                    },
                )
                .join_metadata_result(),
        );
        println!("resp------- {:?}", resp);
        match resp {
            Ok((_, resp, _)) => {
                if resp.status == HealthCheckResponse_ServingStatus::NOT_SERVING {
                    return Err("server is not running. It is not able to start.".to_string());
                }
                debug!("server is running.");
                return Ok(0);
            }
            Err(_resp) => {
                if !server_launched {
                    debug!("server is not running, try to launch the server.");
                    match Command::new(server_name).stdout(Stdio::null()).spawn() {
                        Err(_r) => {
                            return Err("Failed to launch server".to_string());
                        }
                        Ok(_r) => {
                            server_launched = true;

                            //wait server 10 millis
                            thread::sleep(time::Duration::from_millis(100));
                            continue;
                        }
                    };
                } else {
                    return Err("Failed to launch server".to_string());
                }
            }
        };
    }
}

/// Stops the server with a timeout (seconds) specified
/// The timeout value should no larger than the default timeout value (30 seconds)
fn stop_server(client: &OcclumExecClient, time: u32) {
    let time = cmp::min(time, DEFAULT_SERVER_TIMER);
    if let Err(_) = executor::block_on(
        client
            .stop_server(
                grpc::RequestOptions::new(),
                StopRequest {
                    time,
                    ..Default::default()
                },
            )
            .join_metadata_result(),
    ) {
        debug!("The server is not running.");
    } else {
        debug!("The server has received the stop request.");
    }
}

//Gets the application return value
fn get_return_value(client: &OcclumExecClient, process_id: &i32) -> Result<i32, ()> {
    loop {
        let resp = executor::block_on(
            client
                .get_result(
                    grpc::RequestOptions::new(),
                    GetResultRequest {
                        process_id: *process_id,
                        ..Default::default()
                    },
                )
                .join_metadata_result(),
        );
        match resp {
            Ok((_, resp, _)) => {
                if resp.status == GetResultResponse_ExecutionStatus::STOPPED {
                    return Ok(resp.result);
                } else if resp.status == GetResultResponse_ExecutionStatus::RUNNING {
                    thread::sleep(time::Duration::from_millis(100));
                    continue;
                } else {
                    return Err(());
                }
            }
            Err(_) => return Err(()),
        }
    }
}

// Kill the process running in server
fn kill_process(client: &OcclumExecClient, process_id: &i32, signal: &i32) {
    if executor::block_on(
        client
            .kill_process(
                grpc::RequestOptions::new(),
                KillProcessRequest {
                    process_id: *process_id,
                    signal: *signal,
                    ..Default::default()
                },
            )
            .join_metadata_result(),
    )
    .is_err()
    {
        debug!("send signal failed");
    }
}

pub fn run(args: Vec<&str>) -> Result<(), i32> {
    if args.len() < 2 {
        return Err(-1);
    }
    // let env: Vec<String> = env::vars()
    //     .into_iter()
    //     .map(|(key, val)| format!("{}={}", key, val))
    //     .collect();

    // let mut sock_file = String::from(args[0]);
    // let sock_file = str::replace(
    //     sock_file.as_mut_str(),
    //     DEFAULT_CLIENT_FILE,
    //     DEFAULT_SOCK_FILE,
    // );

    let client = OcclumExecClient::new_plain("127.0.0.1", 7878, ClientConf::new())
        .expect("failed to create UDS client");

    if args[1] == "stop" {
        let stop_time = 10;
        stop_server(&client, stop_time);
        println!("server stopped.");
    } else if args[1] == "exec" {
        let mut cmd_args: Vec<&str> = args[2..].to_vec();

        let cmd = cmd_args[0];
        // Change cmd_args[0] from path name to program name
        cmd_args[0] = Path::new(cmd_args[0])
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        // let env: Vec<&str> = env.iter().map(|string| string.as_str()).collect();

        // Create the signal handler
        // let process_killed = Arc::new(Mutex::new(false));
        // let process_killed_clone = Arc::clone(&process_killed);

        println!("cmd {:?},cmd_arg {:?}", cmd, cmd_args);
        let env = vec!["RUST_BACKTRACE=1", "LD_LIBRARY_PATH=/root/occlum_instance/build/lib", "OLDPWD=/root/procjet/occlum/occlum/occlum/build/bin", "SGX_SDK=/opt/intel/sgxsdk", "PWD=/root/occlum_instance", "HOME=/root", "TERM=xterm", "SHLVL=2", "PATH=/opt/occlum/toolchains/jvm/bin:/opt/occlum/toolchains/rust/bin:/opt/occlum/toolchains/golang/bin:/opt/occlum/build/bin:/usr/local/occlum/bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/intel/sgxsdk/bin:/opt/intel/sgxsdk/bin/x64", "OCCLUM_RUST_VERSION=nightly-2020-04-07", "PKG_CONFIG_PATH=:/opt/intel/sgxsdk/pkgconfig", "_=/root/occlum_instance/build/bin/occlum_exec_client"];
        match exec_command(&client, cmd, &cmd_args, &env) {
            Ok(process_id) => {
                // the signal thread exit if server finished execution or user kill the client
                println!("before signal_thread.join().unwrap()----------");
                // signal_thread.join().unwrap();
                println!("process_id:{}", process_id);

                if let Ok(result) = get_return_value(&client, &process_id) {
                    if result != 0 {
                        return Err(result);
                    }
                } else {
                    debug!("get the return value failed");
                    return Err(-1);
                }
                // }
            }
            Err(s) => {
                debug!("execute command failed {}", s);
                return Err(-1);
            }
        };
    } else {
        unreachable!();
    }

    Ok(())
}
