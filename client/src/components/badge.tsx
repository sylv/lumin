import type { FC, ReactNode } from "react";

export const Badge: FC<{ children: ReactNode }> = ({ children }) => (
	<span className="text-xs bg-zinc-800 px-2 py-1 rounded mr-2 shrink-0">
		{children}
	</span>
);
