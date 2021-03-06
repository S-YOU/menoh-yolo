use image;
use menoh;
use ndarray;
use std::cmp;
use std::path;

use bb;
use partial_cmp;

use image::GenericImage;
use model_ext::ModelExt;
use rect::Rect;

#[derive(Deserialize)]
pub struct Config {
    pub input: String,
    pub output: String,
    pub insize: usize,
    pub anchors: Vec<(f32, f32)>,
    pub label_names: Vec<String>,
}

pub struct YOLOv2<'a> {
    model: menoh::Model,
    config: &'a Config,
}

impl<'a> YOLOv2<'a> {
    pub fn from_onnx<P>(
        path: P,
        config: &'a Config,
        backend: &str,
        backend_config: &str,
    ) -> Result<Self, menoh::Error>
    where
        P: AsRef<path::Path>,
    {
        let model = menoh::Builder::from_onnx(path)?
            .add_input::<f32>(&config.input, &[1, 3, config.insize, config.insize])?
            .add_output(&config.output)?
            .build(backend, backend_config)?;
        Ok(Self { model, config })
    }

    pub fn predict(&mut self, img: &image::DynamicImage) -> Result<Vec<bb::Bb>, menoh::Error> {
        let scale = set_image(
            self.model
                .get_view_mut(&self.config.input)?
                .subview_mut(ndarray::Axis(0), 0),
            img,
        );

        self.model.run()?;

        let mut bbox = decode(
            self.model
                .get_view(&self.config.output)?
                .subview(ndarray::Axis(0), 0),
            &self.config.anchors,
            self.config.label_names.len(),
            0.5,
        );
        suppress(&mut bbox, 0.45);

        let scale = self.config.insize as f32 / scale;
        for bb in bbox.iter_mut() {
            bb.top = (bb.top - 0.5) * scale + img.height() as f32 / 2.;
            bb.left = (bb.left - 0.5) * scale + img.width() as f32 / 2.;
            bb.bottom = (bb.bottom - 0.5) * scale + img.height() as f32 / 2.;
            bb.right = (bb.right - 0.5) * scale + img.width() as f32 / 2.;
        }

        Ok(bbox)
    }
}

fn set_image(mut in_: ndarray::ArrayViewMutD<f32>, img: &image::DynamicImage) -> f32 {
    assert_eq!(in_.shape()[0], 3);

    let (in_h, in_w) = (in_.shape()[1], in_.shape()[2]);
    let scale = partial_cmp::min(
        (in_h as f32) / (img.height() as f32),
        (in_w as f32) / (img.width() as f32),
    ).unwrap();
    let img = img.resize(in_h as _, in_w as _, image::FilterType::Nearest);
    let (h, w) = (img.height() as usize, img.width() as usize);

    in_.fill(0.5);
    for c in 0..3 {
        for y in 0..h {
            for x in 0..w {
                in_[[c, y + (in_h - h) / 2, x + (in_w - w) / 2]] =
                    (img.get_pixel(x as _, y as _).data[c] as f32) / 255.;
            }
        }
    }

    scale
}

fn decode(
    out: ndarray::ArrayViewD<f32>,
    anchors: &[(f32, f32)],
    n_fg_class: usize,
    thresh: f32,
) -> Vec<bb::Bb> {
    let (out_h, out_w) = (out.shape()[1], out.shape()[2]);
    let out = out
        .into_shape((anchors.len(), 4 + 1 + n_fg_class, out_h, out_w))
        .unwrap();

    let mut bbox = Vec::new();
    for y in 0..out_h {
        for x in 0..out_w {
            for a in 0..anchors.len() {
                let loc = out.slice(s![a, ..4, y, x]);
                let obj = out[[a, 4, y, x]];
                let conf = out.slice(s![a, 4 + 1.., y, x]);

                let y = y as f32 + sigmoid(loc[0]);
                let x = x as f32 + sigmoid(loc[1]);
                let h = anchors[a].0 * loc[2].exp();
                let w = anchors[a].1 * loc[3].exp();

                let obj = sigmoid(obj);
                let mut score = conf.map(|c| c.exp());
                let sum = score.scalar_sum();
                score.map_inplace(|s| *s *= obj / sum);

                for lb in 0..n_fg_class {
                    if score[lb] >= thresh {
                        bbox.push(bb::Bb {
                            top: (y - h / 2.) / out_h as f32,
                            left: (x - w / 2.) / out_w as f32,
                            bottom: (y + h / 2.) / out_h as f32,
                            right: (x + w / 2.) / out_w as f32,
                            label: lb,
                            score: score[lb],
                        });
                    }
                }
            }
        }
    }
    bbox
}

fn sigmoid(x: f32) -> f32 {
    1. / (1. + (-x).exp())
}

fn suppress(bbox: &mut Vec<bb::Bb>, thresh: f32) {
    bbox.sort_unstable_by(|a, b| {
        a.label.cmp(&b.label).then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(cmp::Ordering::Equal),
        )
    });
    bbox.dedup_by(|a, b| a.label == b.label && a.iou(b) > thresh);
}
