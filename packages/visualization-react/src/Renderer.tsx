import { Children, type ReactNode } from "react";
import {
	JSONUIProvider,
	Renderer,
	type ComponentRenderer,
	type ComponentRenderProps,
} from "@json-render/react";
import {
	ArrowDown,
	CheckCircle2,
	CircleAlert,
	GitBranch,
	HardDrive,
	Info,
} from "lucide-react";
import type { VisualEnvelope } from "./contract";
import type { VisualComponentName, VisualIconName } from "./generated";
import type { LucideIcon } from "lucide-react";
type Props = ComponentRenderProps<Record<string, string>>;
const icons = {
	checkCircle: CheckCircle2,
	circleAlert: CircleAlert,
	hardDrive: HardDrive,
	gitBranch: GitBranch,
	arrowDown: ArrowDown,
} satisfies Record<VisualIconName, LucideIcon>;
function Heading({
	title,
	description,
}: {
	title?: string;
	description?: string;
}) {
	return (
		<>
			{title !== undefined ? <div className="cv-title">{title}</div> : null}
			{description !== undefined ? (
				<div className="cv-description">{description}</div>
			) : null}
		</>
	);
}
function container(name: string) {
	return function Container({ element, children }: Props) {
		return (
			<div className={"cv-" + name}>
				<Heading {...element.props} />
				{children}
			</div>
		);
	};
}
const registry = {
	Card: container("card"),
	Group: container("group"),
	Columns: ({ element, children }: Props) => (
		<div className="cv-columns-group">
			<Heading {...element.props} />
			<div className="cv-columns">{children}</div>
		</div>
	),
	Sequence: ({ element, children }: Props) => (
		<div className="cv-sequence">
			<Heading {...element.props} />
			<ol>
				{Children.map(children, (child: ReactNode, index) => (
					<li>
						<span className="cv-step" aria-hidden="true">
							{index + 1}
						</span>
						<div>{child}</div>
					</li>
				))}
			</ol>
		</div>
	),
	Text: ({ element }: Props) => (
		<div className={"cv-text cv-text-" + element.props.role}>
			{element.props.text}
		</div>
	),
	Status: ({ element }: Props) => {
		const Icon =
			element.props.tone === "success"
				? CheckCircle2
				: ["warning", "error"].includes(element.props.tone)
					? CircleAlert
					: Info;
		return (
			<div className={"cv-status cv-tone-" + element.props.tone}>
				<Icon size={18} aria-hidden="true" />
				<span>{element.props.label}</span>
			</div>
		);
	},
	Badge: ({ element }: Props) => (
		<span className="cv-badge">{element.props.text}</span>
	),
	Icon: ({ element }: Props) => {
		const Icon = icons[element.props.name as keyof typeof icons];
		return <Icon size={20} className="cv-icon" aria-hidden="true" />;
	},
	Divider: () => <hr className="cv-divider" />,
} satisfies Record<
	VisualComponentName,
	ComponentRenderer<Record<string, string>>
>;
export function VisualRenderer({ visual }: { visual: VisualEnvelope }) {
	return (
		<div className="centaeris-visual">
			<JSONUIProvider registry={registry}>
				<Renderer spec={visual.spec} registry={registry} />
			</JSONUIProvider>
		</div>
	);
}
