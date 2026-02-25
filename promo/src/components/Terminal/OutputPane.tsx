import React, { useRef, useEffect } from "react";
import { COLORS, FONTS } from "../../data/brand";
import { interpolate, useCurrentFrame } from "remotion";
import { ConfirmWidget } from "./ConfirmWidget";
import { OutputLine, ConfirmWidgetState } from "../../data/tui-states";

interface OutputPaneProps {
  lines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
  currentFrame?: number; // override useCurrentFrame() for remapped clips
}

export const OutputPane: React.FC<OutputPaneProps> = ({
  lines,
  confirmWidget,
  currentFrame,
}) => {
  const remotionFrame = useCurrentFrame();
  const frame = currentFrame ?? remotionFrame;
  const scrollRef = useRef<HTMLDivElement>(null);

  const visibleLines = lines.filter((l) => frame >= l.appearFrame);

  // Auto-scroll to bottom every render so the latest output is always visible,
  // exactly like a real terminal. overflow:hidden containers still support
  // scrollTop so no scrollbar is shown.
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  });

  return (
    <div
      ref={scrollRef}
      style={{
        fontFamily: FONTS.mono,
        fontSize: 13,
        color: COLORS.textPrimary,
        padding: "8px 14px",
        lineHeight: 1.7,
        height: "100%",
        boxSizing: "border-box",
        overflow: "hidden",
      }}
    >
      {visibleLines.map((line, i) => {
        const lineAge = frame - line.appearFrame;
        const opacity = interpolate(lineAge, [0, 6], [0, 1], {
          extrapolateRight: "clamp",
        });
        const translateY = interpolate(lineAge, [0, 6], [8, 0], {
          extrapolateRight: "clamp",
        });
        return (
          <div
            key={i}
            style={{
              opacity,
              transform: `translateY(${translateY}px)`,
              color: line.color ?? COLORS.textPrimary,
              whiteSpace: "pre",
            }}
          >
            {line.text}
          </div>
        );
      })}

      {/* Confirm widget */}
      {confirmWidget && frame >= confirmWidget.appearFrame && (
        <div
          style={{
            opacity: interpolate(
              frame - confirmWidget.appearFrame,
              [0, 8],
              [0, 1],
              { extrapolateRight: "clamp" }
            ),
          }}
        >
          <ConfirmWidget
            file={confirmWidget.file}
            selected={confirmWidget.selected}
          />
        </div>
      )}
    </div>
  );
};
