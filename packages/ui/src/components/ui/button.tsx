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
        chrome: "bg-transparent text-[#8d96a0] hover:bg-[#e1e6ed] hover:text-[#5f6872]",
        chromeSubtle: "bg-transparent text-[#a9b1ba] hover:bg-[#e1e6ed] hover:text-[#737c86]",
        chromeMenu: "bg-transparent text-[#7f8790] hover:bg-[#e1e6ed] hover:text-[#5f6872]",
        window: "rounded-none border-0 bg-transparent text-[#20262d] shadow-none hover:bg-[#d8dce2] hover:text-[#0f1419]",
        windowDanger: "rounded-none border-0 bg-transparent text-[#20262d] shadow-none hover:bg-[#e81123] hover:text-white",
        workspace: "bg-transparent text-[#747b82] hover:bg-[#f0f2f4] hover:text-[#24282d]",
        workspaceActive: "bg-[#edf0f3] text-[#24282d] hover:bg-[#e7ebef]",
        workspaceChip:
          "border border-[#e1e4e8] bg-[#fbfbfc] text-[#3f454b] shadow-[inset_0_0_0_1px_rgba(255,255,255,0.72)] hover:bg-[#f3f5f7] hover:text-[#24282d]",
        composerIcon: "border border-transparent bg-transparent text-[#6f7780] hover:bg-[#f3f5f7] hover:text-[#24282d]",
        composerChip:
          "border border-[#dfe4ea] bg-[#f5f7f9] text-[#333a41] hover:border-[#d4dbe3] hover:bg-[#eef2f6] hover:text-[#20262d]",
        composerRiskChip:
          "border border-[#f0c878] bg-[#fff5dc] text-[#8a4b00] hover:border-[#e6b861] hover:bg-[#ffefc7] hover:text-[#6f3d00]",
        composerSend: "bg-[#252a31] text-white hover:bg-[#15191e]",
        composerStop: "bg-[#f2f4f6] text-[#24282d] hover:bg-[#e7ebef]",
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
