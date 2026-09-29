'use client';

import { useState, useCallback } from 'react';
import Link from 'next/link';
import { Search, Star, Users, ArrowRight, Sparkles } from 'lucide-react';
import { resolveHandle } from '@/lib/registry';
import { getScores } from '@/lib/reputation';
import { Avatar } from '@/components/Avatar';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { StateArt } from '@/components/ui/state-art';
import { Frame } from '@/components/fx/frame';
import { cn } from '@/lib/utils';

type SearchResult = {
  handle: string;
  address: string;
  social: number;
  earned: number;
} | null;

type SearchState = 'idle' | 'loading' | 'found' | 'not-found' | 'error';

/**
 * People discovery — type a @handle to surface the profile with a vouch shortcut.
 * Registry forward-lookup resolves handle → address, then contract reads pull
 * both XP tracks so the result card shows the full public portrait.
 */
export default function PeoplePage() {
  const [query, setQuery] = useState('');
  const [result, setResult] = useState<SearchResult>(null);
  const [state, setState] = useState<SearchState>('idle');
  const [searched, setSearched] = useState('');

  const search = useCallback(async (term: string) => {
    const h = term.trim().toLowerCase().replace(/^@/, '');
    if (!h) return;

    setSearched(h);
    setState('loading');
    setResult(null);

    try {
      const address = await resolveHandle(h);
      if (!address) {
        setState('not-found');
        return;
      }
      const scores = await getScores(address);
      setResult({ handle: h, address, ...scores });
      setState('found');
    } catch {
      setState('error');
    }
  }, []);

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    void search(query);
  }

  return (
    <div className="grid gap-6">
      <header>
        <h1 className="font-display text-2xl font-semibold">Find people</h1>
        <p className="mt-1 max-w-prose text-sm text-muted-foreground text-balance">
          Search by @handle to find someone&apos;s constellation and vouch for them.
        </p>
      </header>

      {/* Search */}
      <form onSubmit={handleSubmit} className="relative">
        <Search className="absolute left-4 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search by @handle…"
          className="h-12 pl-11 pr-24 font-mono"
          aria-label="Search users by handle"
        />
        <Button
          type="submit"
          variant="flow"
          size="sm"
          disabled={state === 'loading' || query.trim().length === 0}
          className="absolute right-2 top-1/2 -translate-y-1/2"
        >
          {state === 'loading' ? 'Searching…' : 'Find'}
        </Button>
      </form>

      {/* Results */}
      <Frame label={searched ? `search // @${searched}` : 'search // people'} index="01" tape="tl">
        <div className="p-5">
          {state === 'loading' && (
            <div className="flex flex-col gap-3">
              <Skeleton className="h-16 w-full rounded-xl" />
              <Skeleton className="h-4 w-3/4 rounded" />
            </div>
          )}

          {state === 'idle' && (
            <div className="flex flex-col items-center gap-4 py-10 text-center">
              <StateArt kind="empty-leaderboard" size={200} className="opacity-50" />
              <div className="space-y-1">
                <p className="flex items-center justify-center gap-2 font-display text-sm font-medium text-muted-foreground">
                  <Users className="size-4" />
                  Discover the network
                </p>
                <p className="text-xs text-muted-foreground/70">
                  Type an @handle above to find someone&apos;s star.
                </p>
              </div>
            </div>
          )}

          {state === 'not-found' && (
            <div className="flex flex-col items-center gap-4 py-10 text-center">
              <StateArt kind="empty-leaderboard" size={200} className="opacity-50" />
              <div className="space-y-1">
                <p className="font-display text-sm font-medium text-muted-foreground">
                  No one goes by @{searched}
                </p>
                <p className="text-xs text-muted-foreground/70">
                  That handle isn&apos;t claimed yet — maybe they haven&apos;t joined. Try another.
                </p>
              </div>
            </div>
          )}

          {state === 'error' && (
            <div className="flex flex-col items-center gap-4 py-10 text-center">
              <p className="text-sm text-destructive">Something went wrong — try again in a moment.</p>
            </div>
          )}

          {state === 'found' && result && (
            <div className="space-y-4">
              {/* Result card */}
              <div className="flex items-center gap-4 rounded-xl border border-border/60 bg-surface/40 p-4">
                <Avatar
                  address={result.address}
                  handle={result.handle}
                  size={56}
                  ring
                />
                <div className="min-w-0 flex-1">
                  <p className="truncate font-display text-lg font-semibold">
                    @{result.handle}
                  </p>
                  <div className="mt-1.5 flex items-center gap-4 font-mono text-xs text-muted-foreground">
                    <span className="inline-flex items-center gap-1">
                      <Star className={cn('size-3.5', result.social > 0 ? 'text-yellow-400' : 'text-muted-foreground/40')} />
                      {result.social} Social
                    </span>
                    <span className="inline-flex items-center gap-1">
                      <Sparkles className={cn('size-3.5', result.earned > 0 ? 'text-lime' : 'text-muted-foreground/40')} />
                      {result.earned} Earned
                    </span>
                  </div>
                </div>
                <Link href="/app/vouch">
                  <Button variant="flow" size="sm" className="gap-1.5">
                    Vouch
                    <ArrowRight className="size-3.5" />
                  </Button>
                </Link>
              </div>

              {/* Quick link to their public profile */}
              <Link
                href={`/u/${result.handle}`}
                className="inline-flex items-center gap-1.5 text-sm text-primary hover:underline"
              >
                View public profile
                <ArrowRight className="size-3.5" />
              </Link>
            </div>
          )}
        </div>
      </Frame>
    </div>
  );
}
