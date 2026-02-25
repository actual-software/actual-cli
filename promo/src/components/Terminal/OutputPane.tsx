import React from "react";
import { COLORS, FONTS } from "../../data/brand";
import { interpolate, useCurrentFrame } from "remotion";
import { ConfirmWidget } from "./ConfirmWidget";
import { OutputLine, ConfirmWidgetState } from "../../data/tui-states";

interface OutputPaneProps {
  lines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
}

export const OutputPane: React.FC<OutputPaneProps> = ({
  lines,
  confirmWidget,
}) => {
  const frame = useCurrentFrame();

  const visibleLines = lines.filter((l) => frame >= l.appearFrame);

  return (
    <div
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
