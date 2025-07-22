// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

#![doc = include_str!("../README.md")]

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    error::Error,
    hash::{Hash, Hasher},
    time::Duration,
};

use futures::stream::StreamExt;
use libp2p::{
    gossipsub, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, PeerId,
};
use tokio::{io, io::AsyncBufReadExt, select};
use std::io::Write;
use tracing_subscriber::EnvFilter;
use colored::*;
use serde::{Deserialize, Serialize};

// Message types for our chat protocol
#[derive(Serialize, Deserialize, Debug)]
enum ChatMessage {
    Introduction { name: String },
    Text { text: String },
}

// We create a custom network behaviour that combines Gossipsub and Mdns.
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    // Get username from user
    println!("{}", "=== P2P Chat Setup ===".cyan().bold());
    print!("{}", "Enter your name: ".yellow());
    std::io::stdout().flush().unwrap();
    
    let mut name_input = String::new();
    std::io::stdin().read_line(&mut name_input)?;
    let username = name_input.trim().to_string();
    
    if username.is_empty() {
        println!("{}", "Name cannot be empty!".red());
        return Ok(());
    }

    // Store peer names
    let mut peer_names: HashMap<PeerId, String> = HashMap::new();
    // Track if we've introduced ourselves
    let mut introduced = false;

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            // To content-address message, we can take the hash of message and use it as an ID.
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            // Set a custom gossipsub configuration
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
                .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message
                // signing)
                .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
                .build()
                .map_err(io::Error::other)?; // Temporary hack because `build` does not return a proper `std::error::Error`.

            // build a gossipsub network behaviour
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            Ok(MyBehaviour { gossipsub, mdns })
        })?
        .build();

    // Store our own peer ID to distinguish sent vs received messages
    let local_peer_id = *swarm.local_peer_id();

    // Create a Gossipsub topic
    let topic = gossipsub::IdentTopic::new("test-net");
    // subscribes to our topic
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!();
    println!("{}", "=== Chat Started ===".cyan().bold());
    println!("{}", format!("Welcome {}! Type messages and press Enter to send", username).green());
    println!("{}", "Your messages will appear in white, others' messages in green".dimmed());
    println!();

    // Helper function to send messages
    let send_message = |swarm: &mut libp2p::Swarm<MyBehaviour>, topic: &gossipsub::IdentTopic, msg: ChatMessage| {
        if let Ok(serialized) = serde_json::to_string(&msg) {
            if let Err(_e) = swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized.as_bytes()) {
                // println!("{}: {e:?}", "Send error".red());
            }
        }
    };

    // Kick it off
    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                // Check if this is a command to send introduction
                if line.trim() == "/intro" {
                    let intro = ChatMessage::Introduction { name: username.clone() };
                    send_message(&mut swarm, &topic, intro);
                    introduced = true;
                    println!("{}", "Introduction sent!".dimmed());
                } else {
                    // Send introduction first if we haven't yet
                    if !introduced {
                        let intro = ChatMessage::Introduction { name: username.clone() };
                        send_message(&mut swarm, &topic, intro);
                        introduced = true;
                    }
                    
                    let msg = ChatMessage::Text { text: line.clone() };
                    send_message(&mut swarm, &topic, msg);
                    // Display sent message in white
                    println!("{}", format!("{}: {}", username, line).white());
                }
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("{} {}", "📱 Peer connected:".blue(), peer_id.to_string().chars().take(12).collect::<String>().dimmed());
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, _multiaddr) in list {
                        if let Some(name) = peer_names.remove(&peer_id) {
                            println!("{} {} {}", "📱".yellow(), name.yellow(), "disconnected".dimmed());
                        } else {
                            println!("{} {}", "📱 Peer disconnected:".yellow(), peer_id.to_string().chars().take(12).collect::<String>().dimmed());
                        }
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    propagation_source: peer_id,
                    message_id: _id,
                    message,
                })) => {
                    // Only process messages from other peers
                    if peer_id != local_peer_id {
                        if let Ok(msg_str) = String::from_utf8(message.data.to_vec()) {
                            if let Ok(chat_msg) = serde_json::from_str::<ChatMessage>(&msg_str) {
                                match chat_msg {
                                    ChatMessage::Introduction { name } => {
                                        let is_new_peer = !peer_names.contains_key(&peer_id);
                                        peer_names.insert(peer_id, name.clone());
                                        
                                        if is_new_peer {
                                            println!("{} {} {}", "👋".green(), name.green().bold(), "joined the chat".dimmed());
                                            
                                            // Send our introduction back only if this is a new peer
                                            let intro = ChatMessage::Introduction { name: username.clone() };
                                            send_message(&mut swarm, &topic, intro);
                                        }
                                    },
                                    ChatMessage::Text { text } => {
                                        // If we don't know this peer's name, send our introduction
                                        if !peer_names.contains_key(&peer_id) {
                                            let intro = ChatMessage::Introduction { name: username.clone() };
                                            send_message(&mut swarm, &topic, intro);
                                        }
                                        
                                        let display_name = peer_names.get(&peer_id)
                                            .map(|n| n.as_str())
                                            .unwrap_or("Unknown");
                                        println!("{}", format!("{}: {}", display_name, text).green());
                                    }
                                }
                            }
                        }
                    }
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("{} {}", "🌐 Listening on:".green(), address.to_string().dimmed());
                }
                _ => {}
            }
        }
    }
}