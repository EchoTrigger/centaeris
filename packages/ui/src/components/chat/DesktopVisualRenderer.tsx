import { Children, type ReactNode } from "react";
import { ArrowDown } from "lucide-react";
import {
  VisualRenderer,
  visualRegistry,
  type VisualRenderProps,
  type VisualRegistry,
} from "@centaeris/visualization-react/renderer";
import type { VisualEnvelope } from "@centaeris/visualization-react/contract";
import { Card, CardDescription, CardTitle } from "../ui/card";
import { Badge } from "../ui/badge";
import { Separator } from "../ui/separator";
import "../../styles/visualizations.css";

function Heading({ title, description }: Record<string, string>) {
  return <>
    {title !== undefined ? <CardTitle className="cv-title">{title}</CardTitle> : null}
    {description !== undefined ? <CardDescription className="cv-description">{description}</CardDescription> : null}
  </>;
}

const desktopRegistry = {
  ...visualRegistry,
  Card: ({ element, children }: VisualRenderProps) => (
    <Card className="cv-card"><Heading {...element.props} />{children}</Card>
  ),
  Group: ({ element, children }: VisualRenderProps) => (
    <div className="cv-group"><Heading {...element.props} />{children}</div>
  ),
  Sequence: ({ element, children }: VisualRenderProps) => (
    <div className="cv-sequence">
      <Heading {...element.props} />
      <ol>{Children.map(children, (child: ReactNode, index) => <li>{index > 0 ? <ArrowDown className="cv-flow-arrow" size={17} aria-hidden="true" /> : null}<div>{child}</div></li>)}</ol>
    </div>
  ),
  Badge: ({ element }: VisualRenderProps) => <Badge variant="secondary" className="cv-badge">{element.props.text}</Badge>,
  Divider: () => <Separator className="cv-divider" />,
} satisfies VisualRegistry;

// Presentation follows component relationships, independently of text, IDs and
// preview examples. The portable tree remains owned by the Core contract.
function layout(visual: VisualEnvelope): string {
  const elements = visual.spec.elements;
  const root = elements[visual.spec.root];
  const sequence = root.type === "Sequence" ? root : root.children.length === 1 ? elements[root.children[0]] : null;
  if (sequence?.type !== "Sequence" || sequence.children.length === 0) return "groups";
  const steps = sequence.children.map((id) => elements[id]);
  if (steps.some((step) => step.children.length > 0) && steps.every((step) => step.type === "Group" && step.props.description !== undefined &&
    step.children.every((id) => elements[id].type === "Group"))) return "chain";
  if (steps.every((step) => step.type === "Group" && step.props.title !== undefined &&
    step.children.some((id) => elements[id].type === "Text" && elements[id].props.role === "body"))) return "paragraph";
  return "groups";
}

export default function DesktopVisualRenderer({ visual }: { visual: VisualEnvelope }) {
  return <VisualRenderer visual={visual} registry={desktopRegistry} className={"desktop-visual dv-" + layout(visual)} />;
}
