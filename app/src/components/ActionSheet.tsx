// Shared mobile press-and-hold action sheet — a bottom sheet on phones, a
// centered dialog on sm+. Used by the album track rows and playlist entry rows
// (see routes/Album.tsx, routes/PlaylistDetail.tsx). Pass `SheetItem`s as
// children; a "Cancel" footer + tap-outside both dismiss.

import { createPortal } from "react-dom";

export function ActionSheet({
  title,
  subtitle,
  onClose,
  children,
}: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-end justify-center bg-black/60 backdrop-blur-sm sm:items-center sm:p-6"
      onMouseDown={onClose}
      role="dialog"
      aria-modal="true"
      aria-label={title}
    >
      <div
        className="w-full overflow-hidden rounded-t-2xl border border-oct-border-strong bg-oct-panel shadow-2xl sm:max-w-sm sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <header className="border-b border-oct-border px-5 py-3.5">
          <p className="truncate text-sm font-semibold tracking-tight">{title}</p>
          {subtitle && <p className="mt-0.5 truncate font-mono text-[11px] text-oct-subtle">{subtitle}</p>}
        </header>
        <div className="flex flex-col py-1.5">{children}</div>
        <button
          onClick={onClose}
          className="w-full border-t border-oct-border px-5 py-3.5 text-center text-[13px] text-oct-subtle hover:text-oct-text"
        >
          Cancel
        </button>
      </div>
    </div>,
    document.body,
  );
}

export function SheetItem({
  icon,
  label,
  onClick,
  disabled,
  danger,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`flex w-full items-center gap-3 px-5 py-3 text-left text-[13.5px] disabled:opacity-40 ${
        danger
          ? "text-oct-danger hover:bg-oct-offline/15"
          : "text-oct-muted hover:bg-oct-elevated/60 hover:text-oct-text"
      }`}
    >
      <span className="grid w-4 place-items-center">{icon}</span>
      {label}
    </button>
  );
}
