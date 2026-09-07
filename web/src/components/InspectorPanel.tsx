import type { ReactNode } from "react";

export function InspectorPanel({ title, action, children }: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <>
      <header className="inspector-header">
        <h2 id="inspector-title">{title}</h2>
        {action}
      </header>
      <div className="inspector-content">{children}</div>
    </>
  );
}
