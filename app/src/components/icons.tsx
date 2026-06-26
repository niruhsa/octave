// OCTAVE icon set — SVG line/solid icons lifted verbatim from the design
// comps so the app's chrome matches the mocks pixel-for-pixel. All icons use
// a 16×16 viewBox and inherit color via `currentColor`, so size them with a
// `size` prop and color them with a Tailwind `text-*` utility on the parent
// or the icon itself.

import type { SVGProps } from "react";

export type IconProps = SVGProps<SVGSVGElement> & {
  size?: number;
  /** Stroke width for line icons (ignored by solid icons). */
  sw?: number;
};

function Line({ size = 16, sw = 1.3, children, ...rest }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={sw}
      strokeLinecap="round"
      strokeLinejoin="round"
      {...rest}
    >
      {children}
    </svg>
  );
}

function Solid({ size = 16, children, ...rest }: IconProps & { children: React.ReactNode }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" {...rest}>
      {children}
    </svg>
  );
}

export const SearchIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <circle cx="7" cy="7" r="4.2" />
    <path d="M10.5 10.5 14 14" />
  </Line>
);

export const HomeIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M2.5 7 8 2.5 13.5 7M4 6.2v7h8v-7" />
  </Line>
);

export const DiscIcon = (p: IconProps) => (
  <Line {...p}>
    <circle cx="8" cy="8" r="5.5" />
    <circle cx="8" cy="8" r="1.2" fill="currentColor" stroke="none" />
  </Line>
);

export const ArtistIcon = (p: IconProps) => (
  <Line {...p}>
    <circle cx="8" cy="5.5" r="2.6" />
    <path d="M3.5 13c0-2.5 2-4 4.5-4s4.5 1.5 4.5 4" />
  </Line>
);

/** Microphone — podcasts. */
export const PodcastIcon = (p: IconProps) => (
  <Line {...p}>
    <rect x="6" y="1.8" width="4" height="7" rx="2" />
    <path d="M3.8 7.2a4.2 4.2 0 0 0 8.4 0" />
    <path d="M8 11.4V14M6 14h4" />
  </Line>
);

export const SongIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M5.5 11.5V4l7-1.5v7" />
    <circle cx="4" cy="11.5" r="1.6" fill="currentColor" stroke="none" />
    <circle cx="11" cy="9.7" r="1.6" fill="currentColor" stroke="none" />
  </Line>
);

export const PlaylistIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M2.5 4h8M2.5 8h8M2.5 12h5" />
    <circle cx="12.4" cy="11.6" r="1.5" />
    <path d="M13.9 11.6V7" />
  </Line>
);

export const CloudIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M4.6 12.5a2.7 2.7 0 0 1 .35-5.37 3.5 3.5 0 0 1 6.74.9 2.4 2.4 0 0 1-.3 4.47z" />
  </Line>
);

export const CloudOffIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M5.5 12.5h5.6a2.4 2.4 0 0 0 .5-4.75 3.5 3.5 0 0 0-5.1-2.55" />
    <path d="M4.7 7.2a2.7 2.7 0 0 0 .15 5.3" />
    <path d="M2 2 14 14" />
  </Line>
);

export const DownloadIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M8 2.5v7M5.3 6.7 8 9.4l2.7-2.7M3.5 12.5h9" />
  </Line>
);

export const UploadIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M8 9.5v-7M5.3 5.3 8 2.6l2.7 2.7M3.5 12.5h9" />
  </Line>
);

export const GearIcon = (p: IconProps) => (
  <Line sw={1.2} {...p}>
    <circle cx="8" cy="8" r="2.2" />
    <path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.5 3.5l1.1 1.1M11.4 11.4l1.1 1.1M12.5 3.5l-1.1 1.1M4.6 11.4 3.5 12.5" />
  </Line>
);

export const PlusIcon = (p: IconProps) => (
  <Line sw={1.5} {...p}>
    <path d="M8 3v10M3 8h10" />
  </Line>
);

export const ChevronDownIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M4 6 8 10l4-4" />
  </Line>
);

export const CheckIcon = (p: IconProps) => (
  <Line sw={1.6} {...p}>
    <path d="M3 8 6.5 11.5 13 4.5" />
  </Line>
);

export const SyncIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9" />
    <path d="M13.7 3.2v3.1h-3.1" />
  </Line>
);

export const HeartIcon = ({ size = 16, ...rest }: IconProps) => (
  <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" {...rest}>
    <path d="M8 13.3S2.7 9.7 2.7 6.2A2.6 2.6 0 0 1 8 4.9 2.6 2.6 0 0 1 13.3 6.2C13.3 9.7 8 13.3 8 13.3Z" />
  </svg>
);

export const BellIcon = (p: IconProps) => (
  <Line sw={1.3} {...p}>
    <path d="M8 2a3.4 3.4 0 0 0-3.4 3.4c0 3.1-1.2 4.1-1.2 4.1h9.2s-1.2-1-1.2-4.1A3.4 3.4 0 0 0 8 2Z" />
    <path d="M6.6 12.4a1.6 1.6 0 0 0 2.8 0" />
  </Line>
);

export const QueueIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M2 4h9M2 8h9M2 12h6" />
    <circle cx="13" cy="12" r="1.4" />
    <path d="M14.4 12V7.5" />
  </Line>
);

export const VolumeIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M2.5 6v4h2.3L8 12.4v-9L4.8 6z" fill="currentColor" stroke="none" />
    <path d="M10.6 5.6a3.3 3.3 0 0 1 0 4.8" />
  </Line>
);

export const VolumeHiIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M2.5 6v4h2.3L8 12.4v-9L4.8 6z" fill="currentColor" stroke="none" />
    <path d="M10.6 5.6a3.3 3.3 0 0 1 0 4.8M12.6 3.8a6 6 0 0 1 0 8.4" />
  </Line>
);

export const ShuffleIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M2 4h2.5l7 8H14" />
    <path d="M2 12h2.5l7-8H14" />
    <path d="M11.8 2.4 14 4l-2.2 1.6M11.8 10.4 14 12l-2.2 1.6" />
  </Line>
);

export const RepeatIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M3 6.5V5.5A2 2 0 0 1 5 3.5h6l-1.8-1.8M13 9.5v1A2 2 0 0 1 11 12.5H5l1.8 1.8" />
  </Line>
);

export const RepeatOneIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M3 6.5V5.5A2 2 0 0 1 5 3.5h6l-1.8-1.8M13 9.5v1A2 2 0 0 1 11 12.5H5l1.8 1.8" />
    <path d="M7.5 9V7l-1 .7" stroke="currentColor" />
  </Line>
);

export const PlayIcon = ({ size = 16, ...rest }: IconProps) => (
  <Solid size={size} {...rest}>
    <path d="M3.5 2.4v9.2L11 7z" />
  </Solid>
);

export const PauseIcon = ({ size = 16, ...rest }: IconProps) => (
  <Solid size={size} {...rest}>
    <rect x="4" y="3" width="2.4" height="10" rx="0.7" />
    <rect x="9.6" y="3" width="2.4" height="10" rx="0.7" />
  </Solid>
);

export const PrevIcon = ({ size = 16, ...rest }: IconProps) => (
  <Solid size={size} {...rest}>
    <rect x="2.6" y="3" width="1.8" height="10" rx="0.6" />
    <path d="M13 3.4v9.2L6 8z" />
  </Solid>
);

export const NextIcon = ({ size = 16, ...rest }: IconProps) => (
  <Solid size={size} {...rest}>
    <path d="M3 3.4v9.2L10 8z" />
    <rect x="11.6" y="3" width="1.8" height="10" rx="0.6" />
  </Solid>
);

export const SlidersIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M2 4h9M2 8h9M2 12h6" />
    <circle cx="13" cy="4" r="1.4" />
    <circle cx="13" cy="12" r="1.4" />
  </Line>
);

export const TrashIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5" />
  </Line>
);

export const PowerIcon = (p: IconProps) => (
  <Line sw={1.4} {...p}>
    <path d="M8 2v6" />
    <path d="M5 4.5a4.5 4.5 0 1 0 6 0" />
  </Line>
);

export const EditIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M11 2.5l2.5 2.5L5.5 13H3v-2.5z" />
    <path d="M9.5 4.5l2 2" />
  </Line>
);

export const FolderIcon = (p: IconProps) => (
  <Line {...p}>
    <path d="M2 4.5h4l1.2 1.5H14v6.5H2z" />
  </Line>
);

export const MenuIcon = (p: IconProps) => (
  <Line sw={1.5} {...p}>
    <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" />
  </Line>
);

export const KeyIcon = (p: IconProps) => (
  <Line {...p}>
    <circle cx="5.5" cy="6" r="2.5" />
    <path d="M7.3 7.8 13 13.5M11 11.5l1.3-1.3M9.3 9.8l1.3-1.3" />
  </Line>
);
