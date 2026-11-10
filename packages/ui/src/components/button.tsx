// shadcn/ui, Base UI + Nova (MIT). See THIRD_PARTY_NOTICES.md.
import { Button as ButtonPrimitive } from "@base-ui/react/button"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "../utils"

const buttonVariants = cva(
  "group/button inline-flex shrink-0 items-center justify-center rounded-lg border border-transparent bg-clip-padding text-sm font-medium whitespace-nowrap transition-all outline-none select-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 active:not-aria-[haspopup]:translate-y-px disabled:pointer-events-none disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground hover:bg-primary/80",
        solid: "bg-[var(--solid-surface)] text-[var(--solid-text)] hover:opacity-90",
        outline:
          "border-border bg-background hover:bg-muted hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:border-input dark:bg-input/30 dark:hover:bg-input/50",
        secondary:
          "bg-secondary text-secondary-foreground hover:bg-[color-mix(in_oklch,hsl(var(--secondary)),hsl(var(--foreground))_5%)] aria-expanded:bg-secondary aria-expanded:text-secondary-foreground",
        ghost:
          "hover:bg-muted hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:hover:bg-muted/50",
        destructive:
          "bg-destructive/10 text-destructive hover:bg-destructive/20 focus-visible:border-destructive/40 focus-visible:ring-destructive/20 dark:bg-destructive/20 dark:hover:bg-destructive/30 dark:focus-visible:ring-destructive/40",
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
        default:
          "h-8 gap-1.5 px-2.5 has-data-[icon=inline-end]:pr-2 has-data-[icon=inline-start]:pl-2",
        xs: "h-6 gap-1 rounded-[min(var(--radius-md),10px)] px-2 text-xs in-data-[slot=button-group]:rounded-lg has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3",
        sm: "h-7 gap-1 rounded-[min(var(--radius-md),12px)] px-2.5 text-[0.8rem] in-data-[slot=button-group]:rounded-lg has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3.5",
        lg: "h-9 gap-1.5 px-2.5 has-data-[icon=inline-end]:pr-2 has-data-[icon=inline-start]:pl-2",
        icon: "size-8",
        "icon-xs":
          "size-6 rounded-[min(var(--radius-md),10px)] in-data-[slot=button-group]:rounded-lg [&_svg:not([class*='size-'])]:size-3",
        "icon-sm":
          "size-7 rounded-[min(var(--radius-md),12px)] in-data-[slot=button-group]:rounded-lg",
        "icon-lg": "size-9",
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
  }
)

function Button({
  className,
  variant = "default",
  size = "default",
  ...props
}: ButtonPrimitive.Props & VariantProps<typeof buttonVariants>) {
  return (
    <ButtonPrimitive
      data-slot="button"
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}

export { Button, buttonVariants }
