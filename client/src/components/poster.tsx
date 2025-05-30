import clsx from "clsx";
import { ImageIcon } from "lucide-react";
import type { FC } from "react";

interface PosterProps {
	imageUrl: string | null | undefined;
	alt: string;
	className?: string;
}

export const Poster: FC<PosterProps> = ({
	imageUrl,
	alt,
	className = "h-64",
}) => {
	if (!imageUrl) {
		return (
			<div
				className={clsx(
					"rounded-lg aspect-[2/3] from-zinc-800 to-zinc-900 bg-gradient-to-br flex flex-col justify-center items-center gap-2 text-zinc-500 p-4 overflow-hidden shrink-0",
					className,
				)}
			>
				<ImageIcon />
				<span className="text-sm">{alt}</span>
			</div>
		);
	}

	return (
		<img
			src={`/api/image-proxy/${encodeURIComponent(imageUrl)}`}
			alt={alt}
			className={clsx(
				"rounded-lg aspect-[2/3] from-zinc-900 to-zinc-800 bg-gradient-to-b",
				className,
			)}
			loading="lazy"
			decoding="async"
		/>
	);
};
