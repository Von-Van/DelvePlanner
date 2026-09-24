import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";
import { OllamaStatus } from "./api";
import { Glyph, Mark } from "./Geometry";
import { ModelPicker, TESTED_MODEL } from "./ModelPicker";

export function Onboarding({
  status,
  onRefresh,
  onComplete,
  onMessage,
}: {
  status: OllamaStatus | null;
  onRefresh: () => Promise<void>;
  onComplete: () => void;
  onMessage: (message: string) => void;
}) {
  const [step, setStep] = useState(0);
  const [permission, setPermission] = useState<
    "unknown" | "granted" | "denied"
  >("unknown");
  const dialogRef = useRef<HTMLElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  // Each new step starts at its heading, so keyboard and screen reader users aren't left on the
  // button that just disappeared.
  useEffect(() => {
    if (step > 0) headingRef.current?.focus();
  }, [step]);
  useEffect(() => {
    dialogRef.current?.focus();
    void isPermissionGranted().then((value) =>
      setPermission(value ? "granted" : "unknown"),
    );
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || !dialogRef.current) return;
      const items = dialogRef.current.querySelectorAll<HTMLElement>(
        'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]',
      );
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  // A model Delve Planner can use is what "ready" means now; the runtime itself only runs during a
  // request, so an idle runtime is the healthy state rather than something to fix.
  const ready = status?.modelInstalled ?? false;
  const finish = () => {
    try {
      // The key keeps the app's former name, DayPlan, which App reads on launch.
      localStorage.setItem("dayplan-onboarding", "complete");
    } catch {
      // Without storage the welcome returns next launch; Delve Planner still opens now.
    }
    onComplete();
  };
  async function enableNotifications() {
    try {
      const next = await requestPermission();
      setPermission(next === "granted" ? "granted" : "denied");
    } catch (cause) {
      onMessage(cause instanceof Error ? cause.message : String(cause));
    }
  }
  const panels = [
    <div className="onboarding-panel" key="storage">
      <span className="onboarding-index" aria-hidden="true">
        01
      </span>
      <p>STEP 1 OF 3</p>
      <h2 ref={headingRef} tabIndex={-1}>
        Your day stays on this device.
      </h2>
      <div className="onboarding-copy">
        <Mark size={6} />
        <span>
          Events and tasks live in a local SQLite database. There are no
          accounts, sync servers, or cloud AI fallbacks.
        </span>
      </div>
      <button
        className="primary-button onboarding-next"
        onClick={() => setStep(1)}
      >
        Continue <Glyph>→</Glyph>
      </button>
    </div>,
    <div className="onboarding-panel" key="model">
      <span className="onboarding-index" aria-hidden="true">
        02
      </span>
      <p>STEP 2 OF 3</p>
      <h2 ref={headingRef} tabIndex={-1}>
        Your private AI runs inside Delve Planner.
      </h2>
      <div className={`setup-status ${ready ? "ready" : ""}`}>
        <span />
        {ready
          ? "Ready. Delve Planner starts the model only while it answers you."
          : "Choose a local model, or download the one Delve Planner is tested against."}
      </div>
      <p className="onboarding-model-note">
        The Ollama runtime is included. Delve Planner uses models you already
        have, and reads your Ollama folder without changing anything in it.
      </p>
      <ModelPicker
        status={status}
        onRefreshStatus={onRefresh}
        onMessage={onMessage}
      />
      <div className="onboarding-links">
        <button
          onClick={() =>
            void openUrl(`https://ollama.com/library/${TESTED_MODEL}`)
          }
        >
          Model details &amp; license
        </button>
      </div>
      <button
        className="primary-button onboarding-next"
        onClick={() => setStep(2)}
      >
        {ready ? "Model ready" : "Set up later"} <Glyph>→</Glyph>
      </button>
    </div>,
    <div className="onboarding-panel" key="notifications">
      <span className="onboarding-index" aria-hidden="true">
        03
      </span>
      <p>STEP 3 OF 3</p>
      <h2 ref={headingRef} tabIndex={-1}>
        Reminders are optional.
      </h2>
      <div className="onboarding-copy">
        <Mark size={6} />
        <span>
          Delve Planner asks for OS permission only when you enable a reminder.
          The app must remain running in the tray to deliver one.
        </span>
      </div>
      {permission === "granted" ? (
        <div className="permission-ready">
          <Mark size={7} color="var(--success-dot)" filled /> Notifications
          allowed
        </div>
      ) : (
        <button
          className="permission-button"
          onClick={() => void enableNotifications()}
        >
          Allow notifications
        </button>
      )}
      <button className="primary-button onboarding-next" onClick={finish}>
        Open Delve Planner <Glyph>→</Glyph>
      </button>
    </div>,
  ];
  return (
    <div className="onboarding-backdrop">
      <section
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label="Welcome to Delve Planner"
        className="onboarding-card"
      >
        <div className="onboarding-brand">
          <span className="brand-mark" aria-hidden="true">
            <i />
          </span>
          DELVE PLANNER
        </div>
        {panels[step]}
        <div className="onboarding-dots" aria-hidden="true">
          {[0, 1, 2].map((index) => (
            <i key={index} className={index === step ? "active" : ""} />
          ))}
        </div>
      </section>
    </div>
  );
}
