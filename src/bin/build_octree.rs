// Copyright 2016 Google Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[macro_use]
extern crate nom;
extern crate clap;
extern crate byteorder;
extern crate point_viewer;
extern crate scoped_pool;
extern crate pbr;
#[macro_use]
extern crate json;

use point_viewer::Point;
use point_viewer::math::{Vector3f, BoundingBox};
use point_viewer::octree;
use point_viewer::pts::PtsPointStream;

use byteorder::{LittleEndian, WriteBytesExt};
use pbr::{ProgressBar};
use scoped_pool::{Scope, Pool};
use std::collections::{HashSet, HashMap};
use std::fs::{self, File};
use std::io::{BufWriter, Write, Stdout};
use std::cmp;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const UPDATE_COUNT: i64 = 100000;

#[derive(Debug)]
struct NodeWriter {
    writer: BufWriter<File>,
    path: PathBuf,
    num_points: i64,
}

impl Drop for NodeWriter {
    fn drop(&mut self) {
        // If we did not write anything into this node, it should not exist.
        if self.num_points == 0 {
            // We are ignoring deletion errors here in case the file is already gone.
            let _ = fs::remove_file(&self.path);
        }

        // TODO(hrapp): Add some sanity checks that we do not have nodes with ridiculously low
        // amount of points laying around?
    }
}

impl NodeWriter {
    fn new(path: PathBuf) -> Self {
        NodeWriter {
            writer: BufWriter::new(File::create(&path).unwrap()),
            path: path,
            num_points: 0,
        }
    }

    pub fn write(&mut self, p: &Point) {
        self.writer.write_f32::<LittleEndian>(p.position.x).unwrap();
        self.writer.write_f32::<LittleEndian>(p.position.y).unwrap();
        self.writer.write_f32::<LittleEndian>(p.position.z).unwrap();
        self.writer.write_u8(p.r).unwrap();
        self.writer.write_u8(p.g).unwrap();
        self.writer.write_u8(p.b).unwrap();
        self.num_points += 1;
    }
}

struct SplittedNode {
    name: String,
    bounding_box: BoundingBox,
    num_points: i64,
}

fn split<PointIterator: Iterator<Item = Point>>(output_directory: &Path,
                                                name: &str,
                                                bounding_box: &BoundingBox,
                                                stream: PointIterator,
                                                mut progress: Option<SendingProgressReporter>)
                                                -> Vec<SplittedNode> {
    let mut children: Vec<Option<NodeWriter>> = vec![None, None, None, None, None, None, None,
                                                     None];
    for (num_point, p) in stream.enumerate() {
        if num_point % UPDATE_COUNT as usize == 0 {
            progress.as_mut().map(|s| s.add(UPDATE_COUNT as u64));
        }
        let child_index = get_child_index(&bounding_box, &p.position);
        if children[child_index as usize].is_none() {
            children[child_index as usize] =
                Some(NodeWriter::new(octree::node_path(output_directory,
                                               &octree::child_node_name(name, child_index as u8))));
        }
        children[child_index as usize].as_mut().unwrap().write(&p);
    }

    // Remove the node file on disk. This is only save some disk space during processing - all
    // nodes will be rewritten by subsampling the children in the second step anyways. We also
    // ignore file removing error. For example, we never write out the root, so it cannot be
    // removed.
    let _ = fs::remove_file(octree::node_path(output_directory, name));
    let mut rv = Vec::new();
    for (child_index, c) in children.into_iter().enumerate() {
        if c.is_none() {
            continue;
        }
        let c = c.unwrap();

        rv.push(SplittedNode {
            name: octree::child_node_name(name, child_index as u8),
            num_points: c.num_points,
            bounding_box: octree::get_child_bounding_box(&bounding_box, child_index as u8),
        });
    }

    progress.map(|s| s.finish());
    rv
}

fn get_child_index(bounding_box: &BoundingBox, v: &Vector3f) -> u8 {
    let center = bounding_box.center();
    let gt_x = v.x > center.x;
    let gt_y = v.y > center.y;
    let gt_z = v.z > center.z;
    (gt_x as u8) << 2 | (gt_y as u8) << 1 | gt_z as u8
}

struct SendingProgressReporter {
    name: String,
    tx: mpsc::Sender<Status>,
    num_points: i64,
    current_points: i64,
}

impl SendingProgressReporter {
    pub fn new(name: String, tx: mpsc::Sender<Status>, num_points: i64) -> Self {
        let reporter = SendingProgressReporter {
            name: name,
            tx: tx,
            num_points: num_points,
            current_points: 0,
        };
        reporter.send_status();
        reporter
    }

    fn send_status(&self) {
        self.tx.send(Status {
            name: self.name.clone(),
            current_points: self.current_points,
            num_points: self.num_points,
        }).unwrap();
    }

    fn finish(mut self) {
        self.current_points = self.num_points;
        self.send_status();
    }

    fn add(&mut self, count: u64) {
        self.current_points = cmp::min(self.current_points + count as i64, self.num_points);
        self.send_status();
    }
}

fn split_node<'a, 'b: 'a, PointIterator: Iterator<Item = Point>>(scope: &Scope<'a>,
                                                                 output_directory: &'b Path,
                                                                 node: SplittedNode,
                                                                 stream: PointIterator,
                                                                 leaf_nodes_sender: mpsc::Sender<String>,
                                                                 progress_sender: mpsc::Sender<Status>) {
    let progress = stream.size_hint().1.map(|size| {
            SendingProgressReporter::new(
                node.name.clone(), progress_sender.clone(), size as i64)
    });

    let children = split(output_directory, &node.name, &node.bounding_box, stream, progress);
    let (leaf_nodes, split_nodes): (Vec<_>, Vec<_>) = children.into_iter()
        .partition(|n| n.num_points < 100000);

    for child in split_nodes {
        let leaf_nodes_sender_clone = leaf_nodes_sender.clone();
        let progress_sender_clone = progress_sender.clone();
        scope.recurse(move |scope| {
            let stream = octree::PointStream::from_blob(&octree::node_path(output_directory,
                                                                           &child.name));
            split_node(scope, output_directory, child, stream, leaf_nodes_sender_clone, progress_sender_clone);
        });
    }

    for node in leaf_nodes {
        leaf_nodes_sender.send(node.name).unwrap();
    }
}

fn subsample_children_into(output_directory: &Path, node_name: &str) {
    let mut parent = NodeWriter::new(octree::node_path(output_directory, node_name));

    println!("Creating {} from subsampling children...", node_name);
    for i in 0..8 {
        let child_name = octree::child_node_name(node_name, i);
        let path = octree::node_path(output_directory, &child_name);
        if !path.exists() {
            continue;
        }
        let points: Vec<_> = octree::PointStream::from_blob(&path)
            .collect();
        let mut child = NodeWriter::new(octree::node_path(output_directory, &child_name));
        for (idx, p) in points.into_iter().enumerate() {
            if idx % 8 == 0 {
                parent.write(&p);
            } else {
                child.write(&p);
            }
        }

    }
}

#[derive(Debug)]
enum InputFile {
    Ply(PathBuf),
    Pts(PathBuf),
}

#[derive(Debug,Clone)]
struct Status {
    // NOCOM(#hrapp): can this be non-copy?
    name: String,
    current_points: i64,
    num_points: i64,
}

fn make_stream(input: &InputFile) -> (Box<Iterator<Item = Point>>, Option<pbr::ProgressBar<Stdout>>) {
    let stream: Box<Iterator<Item=Point>> = match *input {
        InputFile::Ply(ref filename) => {
            Box::new(octree::PointStream::from_ply(filename))
        }
        InputFile::Pts(ref filename) => Box::new(PtsPointStream::new(filename)),
    };

    let progress_bar = match stream.size_hint().1 {
        Some(size) => Some(ProgressBar::new(size as u64)),
        None => None,
    };
    (stream, progress_bar)
}

fn report_progress(progress_receiver: mpsc::Receiver<Status>, message: &str) {
    let mut progress = HashMap::new();
    while let Ok(status) = progress_receiver.recv() {
        progress.insert(status.name.clone(), format!("{:.2}%",
                     status.current_points as f64 / status.num_points as f64 * 100.));
        if !progress.is_empty() {
            let formatted_progress = &progress.iter().map(|(name, status)| format!("{}({})", name, status)).collect::<Vec<_>>();
            println!("{} {}", message, &formatted_progress.join(", "));
        }

        if status.num_points == status.current_points {
            progress.remove(&status.name);
        }
    }
}

fn main() {
    let matches = clap::App::new("build_octree")
        .args(&[clap::Arg::with_name("output_directory")
                    .help("Output directory to write the octree into.")
                    .long("output_directory")
                    .required(true)
                    .takes_value(true),
                clap::Arg::with_name("input")
                    .help("PLY/PTS file to parse for the points.")
                    .index(1)
                    .required(true)])
        .get_matches();

    let output_directory = &PathBuf::from(matches.value_of("output_directory").unwrap());

    let input = {
        let filename = PathBuf::from(matches.value_of("input").unwrap());
        match filename.extension().and_then(|s| s.to_str()) {
            Some("ply") => InputFile::Ply(filename.clone()),
            Some("pts") => InputFile::Pts(filename.clone()),
            other => panic!("Unknown input file format: {:?}", other),
        }
    };


    let mut num_total_points = 0i64;
    let bounding_box = {
        let mut r = BoundingBox::new();
        let (stream, mut progress_bar) = make_stream(&input);

        if let Some(ref mut progress_bar) = progress_bar {
            progress_bar.message("Determining bounding box: ");
        };

        for p in stream {
            r.update(&p.position);
            num_total_points += 1;
            if num_total_points % UPDATE_COUNT == 0 {
                if let Some(ref mut progress_bar) = progress_bar {
                    progress_bar.add(UPDATE_COUNT as u64);
                }
            }
        }
        r.make_cubic();
        r
    };

    // Ignore errors, maybe directory is already there.
    let _ = fs::create_dir(output_directory);
    let meta = object!{
        "version" => 1,
        "bounding_box" => object!{
            "min_x" => bounding_box.min.x,
            "min_y" => bounding_box.min.y,
            "min_z" => bounding_box.min.z,
            "max_x" => bounding_box.max.x,
            "max_y" => bounding_box.max.y,
            "max_z" => bounding_box.max.z
        }
    };
    File::create(&output_directory.join("meta.json"))
        .unwrap()
        .write_all(&meta.pretty(4).as_bytes())
        .unwrap();

    println!("Creating octree structure.");
    let pool = Pool::new(10);

    let (leaf_nodes_sender, leaf_nodes_receiver) = mpsc::channel();
    let (progress_sender, progress_receiver) = mpsc::channel::<Status>();
    pool.scoped(move |scope| {
        scope.execute(move || {
            report_progress(progress_receiver, "Splitting:");
        });

        let (root_stream, _) = make_stream(&input);
        let root = SplittedNode {
            name: "r".into(),
            bounding_box: bounding_box,
            num_points: num_total_points,
        };
        split_node(scope, output_directory, root, root_stream, leaf_nodes_sender.clone(), progress_sender.clone());
    });

    let mut leaf_nodes: Vec<_> = leaf_nodes_receiver.into_iter().collect();

    // Sort by length of node name, longest first. A node with the same length name as another are
    // on the same tree level and can be subsampled in parallel.
    leaf_nodes.sort_by(|a, b| b.len().cmp(&a.len()));

    while !leaf_nodes.is_empty() {
        let current_length = leaf_nodes[0].len();
        let res = leaf_nodes.into_iter().partition(|n| n.len() == current_length);
        leaf_nodes = res.1;

        let mut parent_names = HashSet::new();
        for node in res.0 {
            let parent_name = octree::parent_node_name(&node);
            if parent_name.is_empty() || parent_names.contains(parent_name) {
                continue;
            }
            parent_names.insert(parent_name.to_string());

            let grand_parent = octree::parent_node_name(&parent_name);
            if !grand_parent.is_empty() {
                leaf_nodes.push(grand_parent.to_string());
            }
        }

        pool.scoped(move |scope| {
            for parent_name in parent_names {
                scope.execute(move || {
                    subsample_children_into(output_directory, &parent_name);
                });
            }
        });
    }
}