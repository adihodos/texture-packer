use clap::Parser;
use image::GenericImageView;
use rectangle_pack::{
    contains_smallest_box, pack_rects, volume_heuristic, GroupedRectsToPlace, RectToInsert,
    TargetBin,
};
use std::collections::BTreeMap;

#[derive(Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct TextureRegion {
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TextureAtlas {
    frames: Vec<TextureRegion>,
    size: (u32, u32),
    file: std::path::PathBuf,
}

#[derive(clap::Parser, Debug)]
struct ProgramOptions {
    #[arg(short, long)]
    input_folders: Vec<std::path::PathBuf>,
    #[arg(short, long)]
    atlas_name: String,
    #[arg(short, long, default_value_t = 2048)]
    sheet_size: u32,
    #[arg(short, long)]
    output_dir: std::path::PathBuf,
}

fn main() {
    let packer_args = ProgramOptions::parse();
    println!("Program args {:?}", packer_args);

    type ImageOutputType = image::ImageBuffer<image::LumaA<u8>, Vec<u8>>;

    let mut rects_to_place = GroupedRectsToPlace::<std::path::PathBuf, &'static str>::new();
    let mut src_img_bytes: BTreeMap<std::path::PathBuf, ImageOutputType> = BTreeMap::new();

    packer_args
        .input_folders
        .iter()
        .filter_map(|path| std::fs::read_dir(path).ok())
        .for_each(|dir_iter| {
            dir_iter
                .filter_map(|de| de.ok().map(|d| d.path()))
                .filter(|de| de.is_file())
                .filter_map(|path| {
                    if let Ok(img) = image::open(path.clone()) {
                        let img = img.to_luma_alpha8();

                        Some((path, img.dimensions(), img))
                    } else {
                        println!("Failed to open image {}", path.display());
                        None
                    }
                })
                .for_each(|(path, dim, bytes)| {
                    rects_to_place.push_rect(
                        path.clone(),
                        None,
                        RectToInsert::new(dim.0, dim.1, 1),
                    );

                    src_img_bytes.insert(path.clone(), bytes);
                });
        });

    let mut target_bins = BTreeMap::new();
    let mut i = 0;
    target_bins.insert(
        format!("atlas{}", i),
        TargetBin::new(packer_args.sheet_size, packer_args.sheet_size, 1),
    );
    i += 1;

    let placement = 'pack_images: loop {
        if let Ok(rect_placements) = pack_rects(
            &rects_to_place,
            &mut target_bins,
            &volume_heuristic,
            &contains_smallest_box,
        ) {
            break 'pack_images rect_placements;
        } else {
            target_bins.clear();

            for j in 0..=i {
                target_bins.insert(
                    format!("atlas{}", j),
                    TargetBin::new(packer_args.sheet_size, packer_args.sheet_size, 1),
                );
            }
            i += 1;

            if target_bins.len() > 32 as usize {
                panic!("Failed to pack, giving up ...");
            }
        }
    };

    let mut idx = 0u32;
    let mut output_images: BTreeMap<String, (ImageOutputType, u32)> = target_bins
        .iter()
        .map(|(atlas_id, _bin_data)| {
            let r = (
                atlas_id.clone(),
                (
                    image::ImageBuffer::new(packer_args.sheet_size, packer_args.sheet_size),
                    idx,
                ),
            );
            idx += 1;
            r
        })
        .collect();

    for (bin_id, loc) in placement.packed_locations() {
        println!("Copying {}", bin_id.display());
        src_img_bytes.get(bin_id).map(|src_bytes| {
            output_images.get_mut(&loc.0).map(|(img, _)| {
                let (_, ploc) = loc;

                for j in 0..src_bytes.height() {
                    for i in 0..src_bytes.width() {
                        img.put_pixel(i + ploc.x(), j + ploc.y(), *src_bytes.get_pixel(i, j));
                    }
                }
            });
        });
    }

    //
    // write individual atlas sheets and merge them into a texture array using toktx
    let mut atlas_sheet_images = output_images
        .iter()
        .map(|(name, (img_buf, idx))| {
            let file_name = format!("{}/{}.png", packer_args.output_dir.to_str().unwrap(), name);
            img_buf
                .save_with_format(&file_name, image::ImageFormat::Png)
                .expect("Failed to save image");

            (file_name, *idx)
        })
        .collect::<Vec<_>>();

    atlas_sheet_images.sort_by_key(|(_, idx)| *idx);

    let mut texture_file_path =
        std::path::Path::new(&packer_args.output_dir).join(&packer_args.atlas_name);
    texture_file_path.set_extension("ktx2");

    let cmd_res = std::process::Command::new("toktx")
        .arg("--layers")
        .arg(atlas_sheet_images.len().to_string())
        .arg("--target_type")
        .arg("RG")
        .arg("--assign_oetf")
        .arg("linear")
        .arg("--t2")
        .arg(texture_file_path.as_path().to_str().unwrap())
        .args(atlas_sheet_images.iter().map(|(fname, _)| fname))
        .output()
        .expect("Failed to create atlas texture array!");

    use std::io::Write;
    std::io::stdout().write_all(&cmd_res.stdout).unwrap();
    std::io::stderr().write_all(&cmd_res.stderr).unwrap();

    if !cmd_res.status.success() {
        println!("toktx failed, exiting ...");
        return;
    }

    //
    // write atlas description file
    let atlas_data = TextureAtlas {
        file: texture_file_path.file_name().unwrap().into(),
        size: (packer_args.sheet_size, packer_args.sheet_size),
        frames: placement
            .packed_locations()
            .iter()
            .filter_map(|(_bin_id, loc_data)| {
                output_images.get(&loc_data.0).map(|&(_, tex_array_id)| {
                    let (_, bin_loc_data) = loc_data;

                    TextureRegion {
                        layer: tex_array_id,
                        x: bin_loc_data.x(),
                        y: bin_loc_data.y(),
                        width: bin_loc_data.width(),
                        height: bin_loc_data.height(),
                    }
                })
            })
            .collect(),
    };

    let mut cfg_file_path =
        std::path::Path::new(&packer_args.output_dir).join(packer_args.atlas_name);
    cfg_file_path.set_extension("ron");

    let mut cfg_outfile = std::io::BufWriter::new(
        std::fs::File::create(cfg_file_path).expect("Failed to write atlas config"),
    );
    cfg_outfile
        .write_all(
            ron::ser::to_string_pretty(&atlas_data, ron::ser::PrettyConfig::new())
                .expect("Failed to serialize atlas data")
                .as_bytes(),
        )
        .expect("Failed to write atlas description file");
}
