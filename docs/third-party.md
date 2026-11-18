# Third-party components

ValoLights' own code is MIT licensed (see [LICENSE](../LICENSE)). The app uses and, in its installer,
redistributes these components, each under its own licence. Licence texts ship in the app folder (`licenses\`,
`bin\`, `assets\…`).

| Component | Used for | Licence | Source |
|---|---|---|---|
| [FFmpeg](https://ffmpeg.org/) 9.0.2, gyan.dev "essentials" build | decoding, encoding, filters | GPL-3.0 (this build) | [gyan.dev](https://www.gyan.dev/ffmpeg/builds/), [GyanD/codexffmpeg](https://github.com/GyanD/codexffmpeg/releases/tag/9.0.2) |
| [libmpv](https://mpv.io/) (shinchiro build 20261111) | video playback in the editor | GPL-2.0-or-later (this build) | [mpv-player/mpv](https://github.com/mpv-player/mpv), [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261111) |
| [rife-ncnn-vulkan](https://github.com/nihui/rife-ncnn-vulkan) 20221029 with the RIFE v4.6 model | AI frame interpolation (super smooth) | MIT | [nihui/rife-ncnn-vulkan](https://github.com/nihui/rife-ncnn-vulkan) |
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) via [whisper-rs](https://github.com/tazz4843/whisper-rs) | voice-chat transcription | MIT / Unlicense | — |
| Whisper large-v3-turbo / small / base (ggml) | speech models, downloaded on first use | MIT | [ggerganov/whisper.cpp on Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) |
| NVIDIA CUDA runtime and cuBLAS 13 | GPU speech-to-text | NVIDIA CUDA EULA (redistributable components) | [NVIDIA](https://docs.nvidia.com/cuda/eula/) |
| [ONNX Runtime](https://onnxruntime.ai/) (via [ort](https://github.com/pykeio/ort)) | OCR and sound models | MIT | — |
| PP-OCRv6 small models ([RapidOCR](https://github.com/RapidAI/RapidOCR)) | reading names in the kill feed | Apache-2.0 | [ModelScope](https://www.modelscope.cn/models/RapidAI/RapidOCR) |
| [YAMNet](https://github.com/tensorflow/models/tree/master/research/audioset/yamnet) (ONNX export) | laughter / shouting detection for Smart cut | Apache-2.0 | [audiomagic/yamnet-onnx](https://huggingface.co/audiomagic/yamnet-onnx) |
| [Inter](https://rsms.me/inter/), [Anton](https://fonts.google.com/specimen/Anton) | UI and subtitle fonts | SIL OFL 1.1 | — |
| [Lucide](https://lucide.dev/) icons | UI icons | ISC | — |
| Microsoft Visual C++ runtime | runtime of the executables | Visual Studio redistributable | — |

FFmpeg and libmpv are separate programs/libraries loaded at run time; their complete source code is available
from the links above. Rust crate dependencies and their licences are listed in `Cargo.lock`
(`cargo tree` / [cargo-about](https://github.com/EmbarkStudios/cargo-about) can produce a full report).

## Riot Games

ValoLights isn't endorsed by Riot Games and doesn't reflect the views or opinions of Riot Games or anyone
officially involved in producing or managing Riot Games properties. Riot Games and all associated properties
are trademarks or registered trademarks of Riot Games, Inc.
