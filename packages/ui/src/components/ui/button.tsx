import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium outline-none transition-colors disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0 focus-visible:ring-1 focus-visible:ring-ring/45 focus-visible:ring-offset-0",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground hover:bg-primary/90",
        destructive: "bg-destructive text-destructive-foreground hover:bg-destructive/90",
        outline: "border border-input bg-background hover:bg-accent hover:text-accent-foreground",
        secondary: "bg-secondary text-secondary-foreground hover:bg-secondary/80",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        link: "text-primary underline-offset-4 hover:underline",
        chrome: "bg-transparent text-[var(--process-text)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        chromeSubtle: "bg-transparent text-[var(--process-text)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        chromeMenu: "bg-transparent text-[var(--process-text)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        window: "rounded-none border-0 bg-transparent text-[var(--process-text)] shadow-none hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        windowDanger: "rounded-none border-0 bg-transparent text-[var(--on-surface)] shadow-none hover:bg-[var(--error)] hover:text-[var(--solid-text)]",
        workspace: "bg-transparent text-[var(--process-text)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        workspaceActive: "bg-[var(--surface-variant)] text-[var(--process-text)] hover:bg-[var(--surface-variant)]",
        workspaceChip:
          "border border-[var(--outline)] bg-[var(--surface-variant)] text-[var(--process-text)]  hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        composerIcon: "border border-transparent bg-transparent text-[var(--process-text)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        composerChip:
          "border border-[var(--outline)] bg-[var(--surface-variant)] text-[var(--process-text)] hover:border-[var(--outline)] hover:bg-[var(--surface-variant)] hover:text-[var(--on-surface)]",
        composerRiskChip:
          "border border-[var(--warning)] bg-[color-mix(in_srgb,var(--warning)_12%,var(--surface-color))] text-[var(--warning)] hover:bg-[color-mix(in_srgb,var(--warning)_20%,var(--surface-color))]",
        composerSend: "bg-[var(--solid-surface)] text-[var(--solid-text)] hover:opacity-90",
        composerStop: "bg-[var(--surface-variant)] text-[var(--process-text)] hover:bg-[var(--surface-variant)]",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 rounded-md px-3 text-xs",
        lg: "h-10 rounded-md px-8",
        icon: "size-8",
        chromeIcon: "size-6 rounded-[10px] p-0",
        chromeMenu: "h-6 w-12 rounded-[5px] px-0 text-xs font-normal",
        window: "h-[34px] w-[52px] rounded-none p-0",
        workspaceIcon: "size-[30px] rounded-[10px] p-0",
        workspaceChip:
          "h-[28px] rounded-lg px-2 text-xs font-normal",
        composerIcon: "size-[30px] rounded-lg p-0",
        composerChip: "h-[26px] rounded-lg px-2 text-[11px] font-medium",
        composerSend: "size-[30px] rounded-full p-0",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

function Button({
  className,
  variant,
  size,
  asChild = false,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean;
  }) {
  const Comp = asChild ? Slot : "button";

  return <Comp className={cn(buttonVariants({ variant, size, className }))} {...props} />;
}

export { Button, buttonVariants };
