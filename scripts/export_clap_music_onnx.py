#!/usr/bin/env python3
"""Export `laion/larger_clap_music` to an ONNX file Octave's server can load.

Octave's ONNX extractor (server/src/services/fingerprint/onnx.rs) feeds the model
a RAW mono waveform:

    input  "audio"  : float32, shape [1, N]   where N = sample_rate * window_secs
                                               (default 48000 * 10 = 480000)
    output           : float32 embedding vector (any length; the server
                       L2-normalizes it and stores the real length per row)

But Hugging Face CLAP does NOT take raw audio — `ClapModel.get_audio_features`
expects `input_features` (a precomputed log-mel spectrogram produced by
`ClapFeatureExtractor`). So this script wraps the model with a mel front-end
implemented in pure, ONNX-exportable torch ops (an STFT expressed as a fixed
Conv1d, then CLAP's own mel filterbank, then log — matching transformers'
`spectrogram(..., power=2.0, log_mel="dB")`), giving a single graph:

        raw waveform [1, N]  ->  log-mel  ->  CLAP audio tower + projection  ->  embedding

Because reproducing a feature extractor by hand is error-prone, the script
**verifies** the exported front-end and the final embedding against the genuine
HF pipeline on a probe signal and refuses to save a model that doesn't match
(override with --force). It auto-detects CLAP's mel filterbank variant
(slaney vs htk) and the input-feature axis order by matching the reference.

Usage
-----
    pip install "torch>=2.1" "transformers>=4.40" onnx numpy
    #   optional, for the post-export ORT check:  pip install onnxruntime
    python scripts/export_clap_music_onnx.py --output larger_clap_music.onnx

Then on the server:
    FINGERPRINT_ENABLED=1
    FINGERPRINT_MODEL=/path/to/larger_clap_music.onnx
    FINGERPRINT_INDEX=pgvector        # optional; ANN backend
    # build the server with the ONNX runtime feature:
    cargo build --release --features onnx
"""

from __future__ import annotations

import argparse
import math
import sys

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F


# ---------------------------------------------------------------------------
# Mel front-end: raw waveform -> log-mel, matching ClapFeatureExtractor.
# ---------------------------------------------------------------------------
class MelFrontend(nn.Module):
    """transformers `spectrogram(waveform, hann, frame_length=n_fft,
    hop_length, power=2.0, mel_filters, log_mel="dB", center=True,
    pad_mode="reflect")` reimplemented with exportable ops.

    The STFT is a fixed Conv1d whose kernels are `window * cos/sin` (so power =
    real^2 + imag^2 == |rfft|^2), then CLAP's mel filterbank as a matmul, then
    `10*log10(max(mel, 1e-10))` (power_to_db with reference=1.0, no top_db).
    """

    def __init__(
        self,
        n_fft: int,
        hop_length: int,
        mel_fb: np.ndarray,   # [n_freq, n_mel]
        transpose_tf: bool,   # emit [.., n_mel, n_frames] instead of [.., n_frames, n_mel]
    ):
        super().__init__()
        self.n_fft = int(n_fft)
        self.hop_length = int(hop_length)
        self.pad = self.n_fft // 2
        self.transpose_tf = transpose_tf

        n_freq = self.n_fft // 2 + 1
        window = torch.hann_window(self.n_fft, periodic=True, dtype=torch.float64)
        k = torch.arange(n_freq, dtype=torch.float64).unsqueeze(1)   # [n_freq, 1]
        n = torch.arange(self.n_fft, dtype=torch.float64).unsqueeze(0)  # [1, n_fft]
        angle = 2.0 * math.pi * k * n / self.n_fft                    # [n_freq, n_fft]
        cos_k = (torch.cos(angle) * window).to(torch.float32)
        sin_k = (torch.sin(angle) * window).to(torch.float32)
        # Conv1d weights: [out=n_freq, in=1, kernel=n_fft]
        self.register_buffer("cos_w", cos_k.unsqueeze(1))
        self.register_buffer("sin_w", sin_k.unsqueeze(1))
        # Mel filterbank [n_freq, n_mel] -> matmul.
        self.register_buffer("mel_fb", torch.from_numpy(mel_fb.astype(np.float32)))

    def forward(self, audio: torch.Tensor) -> torch.Tensor:
        # audio: [1, N] -> [1, 1, N]
        x = audio.unsqueeze(1)
        # center=True, reflect padding (transformers default).
        x = F.pad(x, (self.pad, self.pad), mode="reflect")
        real = F.conv1d(x, self.cos_w, stride=self.hop_length)   # [1, n_freq, T]
        imag = F.conv1d(x, self.sin_w, stride=self.hop_length)   # [1, n_freq, T]
        power = real * real + imag * imag                        # |rfft|^2, [1, n_freq, T]
        # mel = mel_fb.T @ power  ->  [1, T, n_mel]
        mel = torch.einsum("fm,bft->btm", self.mel_fb, power)
        log_mel = 10.0 * torch.log10(torch.clamp(mel, min=1e-10))
        if self.transpose_tf:
            log_mel = log_mel.transpose(1, 2)                    # [1, n_mel, T]
        return log_mel.unsqueeze(1)                              # [1, 1, ?, ?]


class ClapAudioOnnx(nn.Module):
    """raw waveform -> CLAP audio embedding (unnormalized; server L2-normalizes)."""

    def __init__(self, model, frontend: MelFrontend):
        super().__init__()
        self.model = model
        self.frontend = frontend
        # enable_fusion is False for larger_clap_music, so is_longer is unused;
        # a constant False keeps it out of the ONNX input signature.
        self.register_buffer("is_longer", torch.zeros(1, dtype=torch.bool))

    def forward(self, audio: torch.Tensor) -> torch.Tensor:
        feats = self.frontend(audio)
        return clap_audio_embed(self.model, feats, self.is_longer)


# ---------------------------------------------------------------------------
def clap_audio_embed(model, feats: torch.Tensor, is_longer) -> torch.Tensor:
    """The projected CLAP audio embedding, computed via the stock submodules.

    This is exactly what `ClapModel.get_audio_features` does internally
    (`audio_model` -> pooled -> `audio_projection`), but calling the submodules
    directly returns a plain tensor regardless of transformers version — some
    versions wrap `get_audio_features` output in a `ModelOutput` object.
    """
    pooled = model.audio_model(input_features=feats, is_longer=is_longer).pooler_output
    return model.audio_projection(pooled)


def cosine(a: torch.Tensor, b: torch.Tensor) -> float:
    a, b = a.flatten().double(), b.flatten().double()
    return float(torch.dot(a, b) / (a.norm() * b.norm() + 1e-12))


def reference_features(fe, probe: np.ndarray, sr: int):
    """Run the genuine ClapFeatureExtractor, tolerating truncation-arg variance."""
    for kwargs in ({}, {"truncation": "rand_trunc"}, {"truncation": "fusion"}):
        try:
            return fe(probe, sampling_rate=sr, return_tensors="pt", **kwargs)
        except (TypeError, ValueError):
            continue
    raise RuntimeError("could not call ClapFeatureExtractor")


def pick_filterbank_and_orientation(fe, n_fft, hop, probe, sr, ref_feats):
    """Choose the mel filterbank variant + axis order that reproduce the real
    `input_features`, by brute-forcing the small set of candidates."""
    ref = ref_feats["input_features"]  # [1, 1, A, B]
    fbanks = {}
    for attr in ("mel_filters_slaney", "mel_filters"):
        fb = getattr(fe, attr, None)
        if fb is not None:
            fb = np.asarray(fb)
            # transformers stores [n_freq, n_mel]; accept a transposed store too.
            if fb.shape[0] != n_fft // 2 + 1 and fb.shape[1] == n_fft // 2 + 1:
                fb = fb.T
            fbanks[attr] = fb
    if not fbanks:
        raise RuntimeError("no mel filterbank found on the feature extractor")

    audio = torch.from_numpy(probe.astype(np.float32)).unsqueeze(0)
    best = None
    for name, fb in fbanks.items():
        for transpose_tf in (False, True):
            fe_mod = MelFrontend(n_fft, hop, fb, transpose_tf)
            with torch.no_grad():
                got = fe_mod(audio)
            if got.shape != ref.shape:
                continue
            diff = float((got - ref).abs().max())
            if best is None or diff < best[0]:
                best = (diff, name, fb, transpose_tf)
    if best is None:
        raise RuntimeError(
            "no (filterbank, orientation) candidate matched the reference "
            f"input_features shape {tuple(ref.shape)}"
        )
    return best  # (max_abs_diff, name, fb, transpose_tf)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model-id", default="laion/larger_clap_music")
    ap.add_argument("--output", default="larger_clap_music.onnx")
    ap.add_argument("--sample-rate", type=int, default=48000, help="must match onnx.rs MODEL_RATE")
    ap.add_argument("--window-secs", type=int, default=10, help="must match onnx.rs WINDOW_SECS")
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--threshold", type=float, default=0.999, help="min cosine vs HF reference")
    ap.add_argument("--force", action="store_true", help="save even if verification is below threshold")
    args = ap.parse_args()

    from transformers import AutoProcessor, ClapModel  # imported late so --help is instant

    print(f"loading {args.model_id} …")
    model = ClapModel.from_pretrained(args.model_id).eval()
    processor = AutoProcessor.from_pretrained(args.model_id)
    fe = processor.feature_extractor

    sr = args.sample_rate
    n_samples = sr * args.window_secs
    n_fft = int(getattr(fe, "fft_window_size", 1024))
    hop = int(getattr(fe, "hop_length", 480))
    fe_sr = int(getattr(fe, "sampling_rate", sr))
    if fe_sr != sr:
        print(f"WARNING: feature extractor sample_rate={fe_sr} != --sample-rate={sr}; "
              "update onnx.rs MODEL_RATE to match the model.", file=sys.stderr)

    # Deterministic probe: a fixed-length pseudo-random waveform in [-1, 1].
    rng = np.random.default_rng(0)
    probe = (rng.standard_normal(n_samples).astype(np.float32) * 0.1).clip(-1, 1)

    print("resolving mel filterbank + axis order against the real feature extractor …")
    ref_feats = reference_features(fe, probe, sr)
    diff, fb_name, fb, transpose_tf = pick_filterbank_and_orientation(
        fe, n_fft, hop, probe, sr, ref_feats
    )
    print(f"  → filterbank='{fb_name}', transpose_tf={transpose_tf}, "
          f"max|Δ input_features|={diff:.4g}")
    if diff > 1.0:  # dB units; a good match is well under 1 dB
        print("  WARNING: front-end mel does not closely match the reference "
              "(the exported embeddings may be degraded).", file=sys.stderr)

    frontend = MelFrontend(n_fft, hop, fb, transpose_tf)
    wrapper = ClapAudioOnnx(model, frontend).eval()

    # --- Verify the full wrapper against the genuine HF pipeline ---
    # Reference embedding: the genuine feature extractor's mel through the same
    # stock CLAP submodules. Since the front-end mel was already validated
    # against the real extractor above, matching here confirms the whole
    # raw-audio→embedding graph reproduces the HF pipeline end to end.
    audio = torch.from_numpy(probe).unsqueeze(0)  # [1, N]
    with torch.no_grad():
        got = wrapper(audio)
        ref_embed = clap_audio_embed(
            model, ref_feats["input_features"], ref_feats.get("is_longer")
        )
    sim = cosine(got, ref_embed)
    print(f"embedding cosine(wrapper, HF) = {sim:.6f}  (dims={got.shape[-1]})")
    if sim < args.threshold and not args.force:
        print(f"FAILED verification (< {args.threshold}). Not writing the model.\n"
              "  Re-run with --force to export anyway, or open an issue with the\n"
              "  numbers above — likely a transformers-version feature-extractor change.",
              file=sys.stderr)
        return 1

    # --- Export ---
    print(f"exporting → {args.output} (opset {args.opset}) …")
    torch.onnx.export(
        wrapper,
        (audio,),
        args.output,
        input_names=["audio"],
        output_names=["embedding"],
        # Static [1, n_samples] input to match the server contract exactly.
        dynamic_axes=None,
        opset_version=args.opset,
        do_constant_folding=True,
    )

    # --- Optional: confirm the ONNX graph agrees with torch ---
    try:
        import onnxruntime as ort
    except ImportError:
        print("onnxruntime not installed; skipping the ORT parity check.")
    else:
        sess = ort.InferenceSession(args.output, providers=["CPUExecutionProvider"])
        (onnx_out,) = sess.run(None, {"audio": probe[None, :]})
        ort_sim = cosine(torch.from_numpy(onnx_out), got)
        print(f"ORT vs torch cosine = {ort_sim:.6f}")
        if ort_sim < 0.999:
            print("WARNING: ONNX runtime output diverges from torch.", file=sys.stderr)

    print(
        "\nDone. On the server set:\n"
        f"  FINGERPRINT_ENABLED=1\n"
        f"  FINGERPRINT_MODEL={args.output}\n"
        "  (build with `cargo build --release --features onnx`)\n"
        "The extractor's model_version becomes `onnx-<filestem>-512`, so the\n"
        "analysis pass will (re)analyze the library for this model."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
