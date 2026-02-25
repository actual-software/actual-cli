import React from "react";
import { COLORS, FONTS } from "../../data/brand";
import { LogoPanel } from "./LogoPanel";
import { StepsPanel, StepDef } from "./StepsPanel";
import { OutputPane } from "./OutputPane";
import { OutputLine, ConfirmWidgetState } from "../../data/tui-states";

interface TuiLayoutProps {
  steps: StepDef[];
  activeStepIndex: number;
  outputLines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
  glowIntensity?: number;
}

// Ratatui-style bordered panel. Title (if provided) is overlaid on the top border,
// exactly like ratatui's Block::new().title("...").borders(Borders::ALL).
// The outer div is position:relative with NO overflow:hidden so the title at
// top:-8px protrudes above the box border without being clipped. The inner
// content wrapper handles overflow.
const TuiBox: React.FC<{
  title?: React.ReactNode;
  children: React.ReactNode;
  style?: React.CSSProperties;
  contentStyle?: React.CSSProperties;
}> = ({ title, children, style, contentStyle }) => (
  <div
    style={{
      position: "relative",
      border: `1px solid ${COLORS.borderTeal}`,
      borderRadius: 4,
      display: "flex",
      flexDirection: "column",
      ...style,
    }}
  >
    {title && (
      <div
        style={{
          position: "absolute",
          top: -8,
          left: 10,
          background: COLORS.surface,
          padding: "0 4px",
          fontFamily: FONTS.mono,
          fontSize: 12,
          lineHeight: "16px",
          zIndex: 1,
          whiteSpace: "nowrap",
        }}
      >
        {title}
      </div>
    )}
    <div style={{ flex: 1, overflow: "hidden", ...contentStyle }}>
      {children}
    </div>
  </div>
);

// The header title for the bird art box, matching the real CLI first border line:
// ╭ actual v0.1.0 ──── https://app.actual.ai ╮
const LogoBoxTitle = (
  <>
    <span style={{ color: COLORS.textPrimary }}>actual </span>
    <span style={{ color: COLORS.borderGreen }}>v0.1.0</span>
    <span style={{ color: COLORS.textDim }}> ──── </span>
    <span style={{ color: COLORS.borderTeal }}>https://app.actual.ai</span>
  </>
);

export const TuiLayout: React.FC<TuiLayoutProps> = ({
  steps,
  activeStepIndex,
  outputLines,
  confirmWidget,
}) => {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flexGrow: 1,
        background: COLORS.surface,
        color: COLORS.textPrimary,
        // paddingTop: 14px gives the box titles room above their borders.
        // The title is position:absolute at top:-8px, so we need >=8px clearance.
        padding: "14px 8px 8px 8px",
        boxSizing: "border-box",
        overflow: "hidden",
      }}
    >
      {/* Main row: left column + right output box */}
      <div
        style={{
          display: "flex",
          flex: 1,
          gap: 8,
          overflow: "hidden",
          minHeight: 0,
        }}
      >
        {/* Left column: logo box + steps box */}
        <div
          style={{
            width: 330,
            flexShrink: 0,
            display: "flex",
            flexDirection: "column",
            minHeight: 0,
          }}
        >
          {/* Bird art — title is "actual v0.1.0 ──── https://app.actual.ai" */}
          <TuiBox title={LogoBoxTitle} style={{ flex: 1, minHeight: 0 }}>
            <LogoPanel />
          </TuiBox>

          {/* Steps — marginTop:14 gives the "Steps" title room above its border */}
          <TuiBox
            title={<span style={{ color: COLORS.borderTeal }}>Steps</span>}
            style={{ flexShrink: 0, marginTop: 14 }}
            contentStyle={{ overflow: "visible", flex: "none" }}
          >
            <StepsPanel steps={steps} activeStepIndex={activeStepIndex} />
          </TuiBox>
        </div>

        {/* Right column: output */}
        <TuiBox
          title={<span style={{ color: COLORS.borderTeal }}>Output</span>}
          style={{ flex: 1, minHeight: 0 }}
        >
          <OutputPane lines={outputLines} confirmWidget={confirmWidget} />
        </TuiBox>
      </div>

      {/* Footer bar — matches real TUI's bottom key-hint line.
          Shown only when confirm widget is active (← → select  Enter confirm). */}
      {confirmWidget && (
        <div
          style={{
            fontFamily: FONTS.mono,
            fontSize: 11,
            color: COLORS.textDim,
            textAlign: "right",
            paddingTop: 4,
            flexShrink: 0,
          }}
        >
          ← → select{"  "}Enter confirm
        </div>
      )}
    </div>
  );
};
