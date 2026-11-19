import * as Dropdown from "@radix-ui/react-dropdown-menu";
import * as Tooltip from "@radix-ui/react-tooltip";
import { Check } from "lucide-react";
import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");

type Variant = "primary" | "ghost" | "outline" | "danger";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-bg font-semibold hover:brightness-110 disabled:opacity-40",
  ghost: "text-soft hover:text-fg hover:bg-hover disabled:opacity-40",
  outline: "border border-line-active text-fg hover:bg-hover disabled:opacity-40",
  danger: "border border-bad/60 text-bad hover:bg-bad/10 disabled:opacity-40",
};

export const Button = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: "sm" | "md" }>(
  function Button({ variant = "outline", size = "md", className, ...rest }, ref) {
    return (
      <button
        ref={ref}
        {...rest}
        className={cx(
          "inline-flex items-center justify-center gap-2 rounded-md transition-colors select-none",
          size === "sm" ? "h-8 px-3 text-[0.9em]" : "h-10 px-4",
          variants[variant],
          className,
        )}
      />
    );
  },
);

export const IconButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { label: string }>(function IconButton(
  { label, className, children, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      aria-label={label}
      title={label}
      {...rest}
      className={cx(
        "inline-flex size-9 shrink-0 items-center justify-center rounded-md text-soft transition-colors hover:bg-hover hover:text-fg disabled:opacity-40",
        className,
      )}
    >
      {children}
    </button>
  );
});

export function Tip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Tooltip.Root delayDuration={400}>
      <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content
          side="bottom"
          sideOffset={6}
          className="z-50 rounded-md border border-line bg-raised px-2 py-1 text-[0.85em] text-fg2 shadow-lg"
        >
          {label}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}

export const Menu = Dropdown.Root;
export const MenuTrigger = Dropdown.Trigger;

export function MenuContent({ children, align = "end", side = "bottom" }: { children: ReactNode; align?: "start" | "end" | "center"; side?: "top" | "bottom" }) {
  return (
    <Dropdown.Portal>
      <Dropdown.Content className="menu" align={align} side={side} sideOffset={6} collisionPadding={8}>
        {children}
      </Dropdown.Content>
    </Dropdown.Portal>
  );
}

export function MenuItem({ children, onSelect, danger, disabled }: { children: ReactNode; onSelect?: () => void; danger?: boolean; disabled?: boolean }) {
  return (
    <Dropdown.Item className={cx("menu-item", danger && "!text-bad")} onSelect={onSelect} disabled={disabled}>
      {children}
    </Dropdown.Item>
  );
}

/** A choice in a single-select list, with a check on the current one. */
export function MenuChoice({ children, current, onSelect, detail }: { children: ReactNode; current: boolean; onSelect: () => void; detail?: string }) {
  return (
    <Dropdown.Item className="menu-item" onSelect={onSelect} data-current={current || undefined}>
      <span className="flex w-4 shrink-0 justify-center text-accent">{current && <Check size={14} />}</span>
      <span className="flex min-w-0 flex-col">
        <span className={cx(current && "text-accent")}>{children}</span>
        {detail && <span className="truncate text-[0.82em] text-muted">{detail}</span>}
      </span>
    </Dropdown.Item>
  );
}

export const MenuLabel = ({ children }: { children: ReactNode }) => <Dropdown.Label className="menu-label">{children}</Dropdown.Label>;
export const MenuSep = () => <Dropdown.Separator className="menu-sep" />;
/** The blush diamond, pulsing while something is live. */
export function Diamond({ live, className, hollow }: { live?: boolean; className?: string; hollow?: boolean }) {
  return <span className={cx("inline-block w-[1.1em] shrink-0", live && "animate-pulse-soft", className)}>{hollow ? "◇" : "◆"}</span>;
}
