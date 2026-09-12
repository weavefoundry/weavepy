# GC index alignment and empty-dictionary regression

Unimplemented, causal explanation unverified. Compact-GC b63 metadata controls
show retained empty-dictionary time regressions: normal GC medianpaired1.071,
GCdisabled1.148 across9cycles, despiteRSS.975/.974. Candidate normal samples
are bimodal~43/49ms vsbaseline~45; GCdisabledmostly29-31ms vsbaseline25-26.
Lists/sets/tuples improve2-5%. Preserve allrawsamples.

The GC index is FxHashMap<ObjectId,Arc<TrackedHandle>>. ObjectId is u64, dictionary
ids are Arcdata pointers. FxHasher's firstword hash is key*oddSEED; finishreturns
itunchanged. Therefore alignedpointerlowbits remainzero. HashMap bucketgroups
may cluster for regular allocator strides. Onthishost the dictionary Arc has
an80Bpayload+Arcmetadata; narrowinghandle96->80 makes bothpayloads same-sized.
The oldhandles usedadifferentallocatorclass; candidateallocations mayinterleave
withinoneclassanddouble dictionarypointerstride. This is only a hypothesis:
measure actualaddress/hashtableprobingdistribution before blamingtheallocator.

A bounded experiment could give onlyGcState.index a private integer-idhasher
whosefinishis h ^ (h >> 32) after theexistingFxwordfold. This is an invertible
final transformon64bits andmixeshigherbitsintobucketlowbits withoutallocations,
seedchanges, orpointeridentitychanges. Preserve write/write_u64 behavior and
allGcindexlocking/ownership. ComparecurrentFx, finalxor, possiblyrotation using
actualaddress-derivedandsyntheticstrides, thenfullVMallocation/GCcontrols.

DO NOTblindlychangeglobalFxHasher: despiteitsmodule'soldinternal-pointer-only
comment, it also backsPythonDictData,SetData,type-namecachehashing, andnumerous
C-APIregistries. Globalchangehasbroaderdistribution/iteration/order/interner
consequences andneedsaudit. GCindex-onlymixingkeepsthatexperimentbounded.

Also considernarrowfields+paddingdiagnostic toseparateallocatorclass effects
fromopcode/layoutchanges, butneverkeeppaddingsolelytohideasizecost. No hasher
orpaddingprototypehasbeenwritten, run, orapplied. No probabilityclaimfromstatic
hashmathalone. Repeatdictcontrols oncecurrent40290measurementsend.

Completed15cycledictrepeat, before93286fullsuite: normalGCtime1.084/RSS.976,
GCdisabledtime.973/RSS.974. Normalregressionrepeats; disabledswitchesdirection.
Retainbothruns; noprecisecausalclaim. Newdiagnosticmustexplainbimodality, not
onlytheoriginalmedian. See focused/dict-repeat.json/.txt.

Prepared NOTRUN target/inspect_gc_pointer_hashes.py, records100000live dictionary
addresses perprocess on67e7 andb63, GCnormal/disabled, threecycles. Savesactual
addressfiles/hash/sourcebinarysha. Summarizeshomebucketoccupancy at2^17 buckets
forFxandfinalxor. This is NOTactualprobe countsoraperformancecomparison.
Bothbenchmarkstageguards requirecompletionbeforeitwillrun.
