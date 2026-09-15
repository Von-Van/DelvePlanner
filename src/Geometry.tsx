import { CSSProperties } from "react";

export type MarkShape =
  "rhombus" | "square" | "circle" | "ring" | "rule" | "bar";

/**
 * DayPlan's icon system is geometry, not pictograms: one shape per meaning, filled for done or
 * active, outlined for pending, dashed for skipped. Marks are always decorative.
 */
export function Mark({
  shape = "rhombus",
  size = 7,
  color,
  filled = false,
  dashed = false,
  className = "",
}: {
  shape?: MarkShape;
  size?: number;
  color?: string;
  filled?: boolean;
  dashed?: boolean;
  className?: string;
}) {
  const style = {
    "--mark-size": `${size}px`,
    ...(color ? { "--mark-color": color } : {}),
  } as CSSProperties;
  return (
    <i
      aria-hidden="true"
      className={`mark ${shape} ${filled ? "filled" : ""} ${dashed ? "dashed" : ""} ${className}`}
      style={style}
    />
  );
}

/** A turning rhombus for work in progress. */
export function Spinner({ size = 10 }: { size?: number }) {
  return (
    <i
      aria-hidden="true"
      className="spinner"
      style={{ "--mark-size": `${size}px` } as CSSProperties}
    />
  );
}

/** A typographic arrow, cross, or chevron that stands in for an icon inside a labelled control. */
export function Glyph({ children }: { children: string }) {
  return (
    <span className="glyph" aria-hidden="true">
      {children}
    </span>
  );
}
