import { useEffect, useMemo, useState } from "react";
import { btnDangerSm, btnGhostSm, btnPrimary, card, input, label } from "../lib/ui";
import { PlusIcon, TrashIcon } from "../components/icons";
import { ResponseCurve } from "./ResponseCurve";
import {
  cloneEqualizerProfile,
  EQ_LIMITS,
  type EqualizerBand,
  type EqualizerProfile,
} from "./types";
import type { EqualizerGraphDiagnostics } from "../player/audioGraph";

type Props = {
  profile: EqualizerProfile;
  graph: EqualizerGraphDiagnostics | null;
  saving: boolean;
  isNew: boolean;
  previewEnabled: boolean;
  onSave: (profile: EqualizerProfile) => Promise<void>;
  onCancel: () => void;
  onPreview: (profile: EqualizerProfile, immediate?: boolean) => void;
  onStopPreview: () => void;
  onPreviewBypass: (bypassed: boolean) => void;
};

const frequencyToSlider = (frequency: number) =>
  (Math.log(frequency / EQ_LIMITS.frequencyHz.min) /
    Math.log(EQ_LIMITS.frequencyHz.max / EQ_LIMITS.frequencyHz.min)) *
  1_000;
const sliderToFrequency = (slider: number) =>
  EQ_LIMITS.frequencyHz.min *
  (EQ_LIMITS.frequencyHz.max / EQ_LIMITS.frequencyHz.min) ** (slider / 1_000);

function profileError(profile: EqualizerProfile): string | null {
  const name = profile.name.trim();
  if (!name) return "Profile name is required.";
  if ([...name].length > EQ_LIMITS.nameScalars) return "Profile name is too long.";
  if ([...name].some((char) => /[\u0000-\u001f\u007f-\u009f]/u.test(char))) {
    return "Profile name cannot contain control characters.";
  }
  if (profile.bands.length < 1 || profile.bands.length > EQ_LIMITS.bands) {
    return `A profile needs 1 to ${EQ_LIMITS.bands} bands.`;
  }
  const bad = profile.bands.find(
    (band) =>
      !Number.isFinite(band.frequency_hz) ||
      band.frequency_hz < EQ_LIMITS.frequencyHz.min ||
      band.frequency_hz > EQ_LIMITS.frequencyHz.max ||
      !Number.isFinite(band.gain_db) ||
      band.gain_db < EQ_LIMITS.gainDb.min ||
      band.gain_db > EQ_LIMITS.gainDb.max ||
      !Number.isFinite(band.q) ||
      band.q < EQ_LIMITS.q.min ||
      band.q > EQ_LIMITS.q.max,
  );
  if (bad) return `Band ${bad.position} has an out-of-range value.`;
  if (
    !Number.isFinite(profile.preamp_db) ||
    profile.preamp_db < EQ_LIMITS.preampDb.min ||
    profile.preamp_db > EQ_LIMITS.preampDb.max
  ) {
    return `Preamp must be ${EQ_LIMITS.preampDb.min} to +${EQ_LIMITS.preampDb.max} dB.`;
  }
  return null;
}

export function ProfileEditor({
  profile,
  graph,
  saving,
  isNew,
  previewEnabled,
  onSave,
  onCancel,
  onPreview,
  onStopPreview,
  onPreviewBypass,
}: Props) {
  const [draft, setDraft] = useState(() => cloneEqualizerProfile(profile));
  const [previewing, setPreviewing] = useState(false);

  useEffect(() => {
    setDraft(cloneEqualizerProfile(profile));
    setPreviewing(false);
  }, [profile.id, profile.revision]);

  useEffect(
    () => () => {
      onPreviewBypass(false);
      onStopPreview();
    },
    [onPreviewBypass, onStopPreview],
  );

  useEffect(() => {
    if (!previewEnabled && previewing) {
      setPreviewing(false);
      onPreviewBypass(false);
      onStopPreview();
    }
  }, [onPreviewBypass, onStopPreview, previewEnabled, previewing]);

  const error = useMemo(() => profileError(draft), [draft]);
  const dirty = useMemo(
    () => JSON.stringify(draft) !== JSON.stringify(profile),
    [draft, profile],
  );

  const change = (recipe: (next: EqualizerProfile) => void, immediate = false) => {
    const next = cloneEqualizerProfile(draft);
    recipe(next);
    next.bands.forEach((band, index) => {
      band.position = index + 1;
    });
    setDraft(next);
    if (previewing) onPreview(next, immediate);
  };

  const changeBand = (index: number, patch: Partial<EqualizerBand>, immediate = false) =>
    change((next) => Object.assign(next.bands[index], patch), immediate);

  const startPreview = () => {
    setPreviewing(true);
    onPreview(draft, true);
  };

  const stopPreview = () => {
    setPreviewing(false);
    onPreviewBypass(false);
    onStopPreview();
  };

  return (
    <form
      className="flex min-w-0 flex-col gap-5"
      onSubmit={(event) => {
        event.preventDefault();
        if (!error) void onSave({ ...draft, name: draft.name.trim() });
      }}
    >
      <div className="flex flex-wrap items-end gap-3">
        <label className="min-w-[220px] flex-1">
          <span className={label}>PROFILE NAME</span>
          <input
            className={`${input} mt-2`}
            value={draft.name}
            maxLength={EQ_LIMITS.nameScalars}
            onChange={(event) => change((next) => void (next.name = event.target.value))}
            aria-label="Equalizer profile name"
          />
        </label>
        {!previewing ? (
          <button
            type="button"
            onClick={startPreview}
            className={btnGhostSm}
            disabled={!previewEnabled}
            title={previewEnabled ? "Preview this unsaved profile" : "Turn on the Equalizer master switch to preview"}
          >
            Preview
          </button>
        ) : (
          <>
            <span className="rounded-full bg-oct-accent/10 px-2.5 py-1 text-[11px] text-oct-accent">
              Previewing
            </span>
            <button
              type="button"
              className={btnGhostSm}
              onPointerDown={() => onPreviewBypass(true)}
              onPointerUp={() => onPreviewBypass(false)}
              onPointerCancel={() => onPreviewBypass(false)}
              onPointerLeave={() => onPreviewBypass(false)}
              onKeyDown={(event) => {
                if (event.key === " " || event.key === "Enter") onPreviewBypass(true);
              }}
              onKeyUp={() => onPreviewBypass(false)}
              title="Hold to compare against Flat"
            >
              Hold for A/B Flat
            </button>
            <button type="button" onClick={stopPreview} className={btnGhostSm}>
              Stop preview
            </button>
          </>
        )}
        {!previewEnabled && (
          <span className="text-[10.5px] text-oct-faint">Turn on the Equalizer master switch to preview.</span>
        )}
      </div>

      <ResponseCurve profile={draft} sampleRate={graph?.sampleRate} runtime={graph} />

      <div className="flex flex-col gap-2">
        <div className={label}>LEVEL &amp; HEADROOM</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <RangeNumberRow
            label="Preamp"
            description="Stored gain before the filters."
            value={draft.preamp_db}
            min={EQ_LIMITS.preampDb.min}
            max={EQ_LIMITS.preampDb.max}
            step={0.1}
            unit="dB"
            onChange={(value, immediate) => change((next) => void (next.preamp_db = value), immediate)}
          />
          <ToggleRow
            title="Auto headroom (reduces clipping risk)"
            description="Attenuates modeled boosts with a 1 dB steady-state margin. This is not a limiter or safety guarantee."
            checked={draft.auto_headroom_enabled}
            onChange={(checked) =>
              change((next) => void (next.auto_headroom_enabled = checked), true)
            }
          />
        </div>
      </div>

      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <div>
            <div className={label}>PARAMETRIC BANDS</div>
            <div className="mt-1 text-[11px] text-oct-faint">
              {draft.bands.length} of {EQ_LIMITS.bands} peaking filters
            </div>
          </div>
          <button
            type="button"
            className={btnGhostSm}
            disabled={draft.bands.length >= EQ_LIMITS.bands}
            onClick={() =>
              change((next) => {
                const previous = next.bands[next.bands.length - 1];
                next.bands.push({
                  position: next.bands.length + 1,
                  enabled: true,
                  filter_kind: "peaking",
                  frequency_hz: Math.min((previous?.frequency_hz ?? 500) * 2, 20_000),
                  gain_db: 0,
                  q: 1,
                });
              }, true)
            }
          >
            <PlusIcon size={13} /> Add band
          </button>
        </div>

        <div className="flex flex-col gap-2">
          {draft.bands.map((band, index) => (
            <BandEditor
              key={`${draft.id}-${index}`}
              band={band}
              index={index}
              count={draft.bands.length}
              onChange={(patch, immediate) => changeBand(index, patch, immediate)}
              onMove={(direction) =>
                change((next) => {
                  const target = index + direction;
                  if (target < 0 || target >= next.bands.length) return;
                  [next.bands[index], next.bands[target]] = [next.bands[target], next.bands[index]];
                }, true)
              }
              onRemove={() =>
                change((next) => {
                  if (next.bands.length > 1) next.bands.splice(index, 1);
                }, true)
              }
            />
          ))}
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-[12px] text-oct-danger">
          {error}
        </div>
      )}

      <div className="sticky bottom-0 -mx-1 flex flex-wrap justify-end gap-2 border-t border-oct-border bg-oct-bg/95 px-1 py-3 backdrop-blur">
        <button
          type="button"
          className={btnGhostSm}
          onClick={() =>
            change((next) => {
              next.preamp_db = 0;
              next.bands.forEach((band) => {
                band.gain_db = 0;
              });
            }, true)
          }
        >
          Reset Flat
        </button>
        <button
          type="button"
          className={btnGhostSm}
          onClick={() => {
            stopPreview();
            onCancel();
          }}
        >
          Cancel
        </button>
        <button type="submit" className={btnPrimary} disabled={!dirty || !!error || saving}>
          {saving ? "Saving…" : isNew ? "Create profile" : "Save changes"}
        </button>
      </div>
    </form>
  );
}

function BandEditor({
  band,
  index,
  count,
  onChange,
  onMove,
  onRemove,
}: {
  band: EqualizerBand;
  index: number;
  count: number;
  onChange: (patch: Partial<EqualizerBand>, immediate?: boolean) => void;
  onMove: (direction: -1 | 1) => void;
  onRemove: () => void;
}) {
  return (
    <div className={`${card} flex flex-col gap-3 p-3`}>
      <div className="flex items-center gap-2">
        <button
          type="button"
          role="switch"
          aria-checked={band.enabled}
          aria-label={`Enable band ${index + 1}`}
          onClick={() => onChange({ enabled: !band.enabled }, true)}
          className={`inline-flex h-5 w-9 items-center rounded-full px-0.5 transition-colors ${
            band.enabled ? "bg-oct-accent" : "bg-oct-border-strong"
          }`}
        >
          <span
            className={`h-4 w-4 rounded-full bg-white transition-transform ${
              band.enabled ? "translate-x-4" : "translate-x-0"
            }`}
          />
        </button>
        <span className="font-mono text-[12px] text-oct-text">Band {index + 1}</span>
        <span className="rounded bg-oct-elevated px-1.5 py-0.5 font-mono text-[9px] text-oct-faint">
          PK
        </span>
        <div className="ml-auto flex items-center gap-1">
          <button type="button" className={btnGhostSm} disabled={index === 0} onClick={() => onMove(-1)} aria-label={`Move band ${index + 1} up`}>
            ↑
          </button>
          <button type="button" className={btnGhostSm} disabled={index === count - 1} onClick={() => onMove(1)} aria-label={`Move band ${index + 1} down`}>
            ↓
          </button>
          <button type="button" className={btnDangerSm} disabled={count === 1} onClick={onRemove} aria-label={`Remove band ${index + 1}`}>
            <TrashIcon size={12} />
          </button>
        </div>
      </div>

      <div className={`grid gap-3 md:grid-cols-3 ${band.enabled ? "" : "opacity-45"}`}>
        <LogFrequencyRow band={band} index={index} onChange={onChange} />
        <CompactRange
          label="Gain"
          unit="dB"
          value={band.gain_db}
          min={EQ_LIMITS.gainDb.min}
          max={EQ_LIMITS.gainDb.max}
          step={0.1}
          disabled={!band.enabled}
          onChange={(value, immediate) => onChange({ gain_db: value }, immediate)}
          ariaLabel={`Band ${index + 1} gain in decibels`}
        />
        <CompactRange
          label="Q"
          unit=""
          value={band.q}
          min={EQ_LIMITS.q.min}
          max={EQ_LIMITS.q.max}
          step={0.1}
          disabled={!band.enabled}
          onChange={(value, immediate) => onChange({ q: value }, immediate)}
          ariaLabel={`Band ${index + 1} Q`}
        />
      </div>
    </div>
  );
}

function LogFrequencyRow({
  band,
  index,
  onChange,
}: {
  band: EqualizerBand;
  index: number;
  onChange: (patch: Partial<EqualizerBand>, immediate?: boolean) => void;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between gap-2 text-[11px] text-oct-faint">
        <span>Frequency</span>
        <label className="flex items-center gap-1">
          <input
            type="number"
            min={EQ_LIMITS.frequencyHz.min}
            max={EQ_LIMITS.frequencyHz.max}
            step={1}
            value={Number(band.frequency_hz.toFixed(2))}
            disabled={!band.enabled}
            onChange={(event) => onChange({ frequency_hz: Number(event.target.value) })}
            onBlur={() => onChange({}, true)}
            className="w-20 rounded border border-oct-border-strong bg-oct-elevated px-1.5 py-1 text-right font-mono text-[11px] text-oct-text focus:border-oct-accent focus:outline-none"
            aria-label={`Band ${index + 1} frequency in hertz`}
          />
          <span>Hz</span>
        </label>
      </div>
      <input
        type="range"
        min={0}
        max={1_000}
        step={1}
        value={frequencyToSlider(band.frequency_hz)}
        disabled={!band.enabled}
        onChange={(event) =>
          onChange({ frequency_hz: Number(sliderToFrequency(Number(event.target.value)).toFixed(2)) })
        }
        onPointerUp={() => onChange({}, true)}
        onKeyUp={() => onChange({}, true)}
        className="oct-range disabled:opacity-40"
        aria-label={`Band ${index + 1} frequency`}
      />
    </div>
  );
}

function CompactRange({
  label: title,
  unit,
  value,
  min,
  max,
  step,
  disabled,
  onChange,
  ariaLabel,
}: {
  label: string;
  unit: string;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled: boolean;
  onChange: (value: number, immediate?: boolean) => void;
  ariaLabel: string;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between gap-2 text-[11px] text-oct-faint">
        <span>{title}</span>
        <label className="flex items-center gap-1">
          <input
            type="number"
            min={min}
            max={max}
            step={step}
            value={value}
            disabled={disabled}
            onChange={(event) => onChange(Number(event.target.value))}
            onBlur={() => onChange(value, true)}
            className="w-16 rounded border border-oct-border-strong bg-oct-elevated px-1.5 py-1 text-right font-mono text-[11px] text-oct-text focus:border-oct-accent focus:outline-none"
            aria-label={ariaLabel}
          />
          {unit && <span>{unit}</span>}
        </label>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(Number(event.target.value))}
        onPointerUp={() => onChange(value, true)}
        onKeyUp={() => onChange(value, true)}
        className="oct-range disabled:opacity-40"
        aria-label={ariaLabel}
      />
    </div>
  );
}

function RangeNumberRow({
  label: title,
  description,
  value,
  min,
  max,
  step,
  unit,
  onChange,
}: {
  label: string;
  description: string;
  value: number;
  min: number;
  max: number;
  step: number;
  unit: string;
  onChange: (value: number, immediate?: boolean) => void;
}) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <div>
          <div className="text-[13.5px] text-oct-text">{title}</div>
          <div className="text-[11.5px] text-oct-faint">{description}</div>
        </div>
        <label className="flex items-center gap-1 text-[11px] text-oct-faint">
          <input
            type="number"
            min={min}
            max={max}
            step={step}
            value={value}
            onChange={(event) => onChange(Number(event.target.value))}
            onBlur={() => onChange(value, true)}
            className="w-20 rounded-lg border border-oct-border-strong bg-oct-elevated px-2 py-1.5 text-right font-mono text-[12px] text-oct-text focus:border-oct-accent focus:outline-none"
            aria-label={`${title} in ${unit}`}
          />
          {unit}
        </label>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
        onPointerUp={() => onChange(value, true)}
        onKeyUp={() => onChange(value, true)}
        className="oct-range"
        aria-label={`${title} in ${unit}`}
      />
    </div>
  );
}

function ToggleRow({
  title,
  description,
  checked,
  onChange,
}: {
  title: string;
  description: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-4 px-4 py-3">
      <div>
        <div className="text-[13.5px] text-oct-text">{title}</div>
        <div className="text-[11.5px] text-oct-faint">{description}</div>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`inline-flex h-5 w-9 shrink-0 items-center rounded-full px-0.5 transition-colors ${
          checked ? "bg-oct-accent" : "bg-oct-border-strong"
        }`}
      >
        <span
          className={`h-4 w-4 rounded-full bg-white transition-transform ${
            checked ? "translate-x-4" : "translate-x-0"
          }`}
        />
      </button>
    </div>
  );
}
