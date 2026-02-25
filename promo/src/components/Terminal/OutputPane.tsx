import React from "react";
import { COLORS, FONTS } from "../../data/brand";
import { interpolate, useCurrentFrame } from "remotion";
import { ConfirmWidget } from "./ConfirmWidget";
import { OutputLine, ConfirmWidgetState } from "../../data/tui-states";

interface OutputPaneProps {
  lines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
  title?: string;
}

export const OutputPane: React.FC<OutputPaneProps> = ({
  lines,
  confirmWidget,
  title = "Output",
}) => {
  const frame = useCurrentFrame();

  const visibleLines = lines.filter((l) => frame >= l.appearFrame);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        borderLeft: `1px solid ${COLORS.borderTeal}44`,
      }}
    >
      {/* Pane header */}
      <div
        style={{
          fontFamily: FONTS.mono,
          fontSize: 12,
          color: COLORS.textDim,
          padding: "4px 12px",
          borderBottom: `1px solid ${COLORS.borderTeal}22`,
          flexShrink: 0,
        }}
      >
        {title}
      </div>

      {/* Content */}
      <div
        style={{
          fontFamily: FONTS.mono,
          fontSize: 13,
          color: COLORS.textPrimary,
          padding: "8px 14px",
          lineHeight: 1.7,
          flexGrow: 1,
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

      {/* Key hint bar */}
      <div
        style={{
          fontFamily: FONTS.mono,
          fontSize: 11,
          color: COLORS.textDim,
          padding: "3px 14px",
          borderTop: `1px solid ${COLORS.borderTeal}22`,
          flexShrink: 0,
        }}
      >
        ↑/↓ steps  u/d scroll  g/G top/bottom  q quit
      </div>
    </div>
  );
};
