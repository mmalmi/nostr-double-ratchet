---- MODULE DirectMessageSubscriptions ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Authors,
    InitialMessageAuthors,
    InitialSkippedMessageAuthors,
    UpdatedMessageAuthors,
    UpdatedSkippedMessageAuthors,
    BugSessionManagerOwnsDirectSubscriptions,
    BugSkipDesiredRefresh,
    BugReportDesiredAsApplied,
    BugApplyNeverClearsInFlight,
    BugCatchupUsesAppliedPlan,
    BugSessionManagerOmitsSkippedAuthors,
    BugThrottleAddedAuthors

ASSUME Authors # {}
ASSUME InitialMessageAuthors \subseteq Authors
ASSUME InitialSkippedMessageAuthors \subseteq Authors
ASSUME UpdatedMessageAuthors \subseteq Authors
ASSUME UpdatedSkippedMessageAuthors \subseteq Authors
ASSUME BugSessionManagerOwnsDirectSubscriptions \in BOOLEAN
ASSUME BugSkipDesiredRefresh \in BOOLEAN
ASSUME BugReportDesiredAsApplied \in BOOLEAN
ASSUME BugApplyNeverClearsInFlight \in BOOLEAN
ASSUME BugCatchupUsesAppliedPlan \in BOOLEAN
ASSUME BugSessionManagerOmitsSkippedAuthors \in BOOLEAN
ASSUME BugThrottleAddedAuthors \in BOOLEAN

TrackedAuthors(chainAuthors, skippedAuthors) ==
    IF BugSessionManagerOmitsSkippedAuthors
        THEN chainAuthors
        ELSE chainAuthors \cup skippedAuthors

VARIABLES
    managerChainAuthors,
    managerSkippedAuthors,
    managerAuthors,
    managerDirectSubscriptionAuthors,
    desiredPlan,
    applyingPlan,
    appliedPlan,
    refreshDirty,
    refreshInFlight,
    connectRequested,
    applyFailureDone,
    syncedOnce,
    changeDone,
    relayUp,
    relayBag,
    published,
    delivered,
    badAppliedBeforeSuccess,
    badApplyStuckAfterFailure

vars ==
    <<managerChainAuthors, managerSkippedAuthors, managerAuthors,
      managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
      refreshDirty, refreshInFlight, connectRequested, applyFailureDone,
      syncedOnce, changeDone, relayUp, relayBag, published, delivered,
      badAppliedBeforeSuccess, badApplyStuckAfterFailure>>

ApplyTarget ==
    IF BugCatchupUsesAppliedPlan THEN appliedPlan ELSE desiredPlan

InitialTracked ==
    TrackedAuthors(InitialMessageAuthors, InitialSkippedMessageAuthors)

UpdatedTracked ==
    TrackedAuthors(UpdatedMessageAuthors, UpdatedSkippedMessageAuthors)

InitialDesired ==
    IF BugSkipDesiredRefresh THEN {} ELSE InitialTracked

InitialApplied ==
    IF BugReportDesiredAsApplied THEN InitialDesired ELSE {}

Init ==
    /\ managerChainAuthors = InitialMessageAuthors
    /\ managerSkippedAuthors = InitialSkippedMessageAuthors
    /\ managerAuthors = InitialTracked
    /\ managerDirectSubscriptionAuthors =
        IF BugSessionManagerOwnsDirectSubscriptions THEN InitialTracked ELSE {}
    /\ desiredPlan = InitialDesired
    /\ applyingPlan = {}
    /\ appliedPlan = InitialApplied
    /\ refreshDirty = (InitialDesired # InitialApplied)
    /\ refreshInFlight = FALSE
    /\ connectRequested = FALSE
    /\ applyFailureDone = FALSE
    /\ syncedOnce = (InitialDesired = InitialApplied)
    /\ changeDone = FALSE
    /\ relayUp = TRUE
    /\ relayBag = {}
    /\ published = {}
    /\ delivered = {}
    /\ badAppliedBeforeSuccess = (BugReportDesiredAsApplied /\ InitialDesired # {})
    /\ badApplyStuckAfterFailure = FALSE

SessionStateChanges ==
    /\ ~changeDone
    /\ ~refreshDirty
    /\ ~refreshInFlight
    /\ appliedPlan = desiredPlan
    /\ LET nextAuthors == UpdatedTracked
           addedAuthors == nextAuthors \ desiredPlan
           nextDesired ==
               IF BugSkipDesiredRefresh
                   THEN desiredPlan
                   ELSE IF addedAuthors # {} /\ BugThrottleAddedAuthors
                       THEN desiredPlan
                       ELSE nextAuthors
       IN
       /\ managerChainAuthors' = UpdatedMessageAuthors
       /\ managerSkippedAuthors' = UpdatedSkippedMessageAuthors
       /\ managerAuthors' = nextAuthors
       /\ managerDirectSubscriptionAuthors' =
           IF BugSessionManagerOwnsDirectSubscriptions THEN nextAuthors ELSE {}
       /\ desiredPlan' = nextDesired
       /\ refreshDirty' = (nextDesired # appliedPlan)
    /\ applyingPlan' = applyingPlan
    /\ appliedPlan' = appliedPlan
    /\ refreshInFlight' = FALSE
    /\ connectRequested' = connectRequested
    /\ applyFailureDone' = FALSE
    /\ syncedOnce' = syncedOnce
    /\ changeDone' = TRUE
    /\ UNCHANGED <<relayUp, relayBag, published, delivered, badAppliedBeforeSuccess, badApplyStuckAfterFailure>>

RequestConnection ==
    /\ refreshDirty
    /\ ~refreshInFlight
    /\ ~relayUp
    /\ ~connectRequested
    /\ connectRequested' = TRUE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, applyFailureDone, syncedOnce,
        changeDone, relayUp, relayBag, published, delivered, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

ConnectRelay ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ connectRequested' = FALSE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, applyFailureDone, syncedOnce,
        changeDone, relayBag, published, delivered, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

StartApply ==
    /\ refreshDirty
    /\ ~refreshInFlight
    /\ relayUp
    /\ applyingPlan' = desiredPlan
    /\ refreshInFlight' = TRUE
    /\ refreshDirty' = FALSE
    /\ IF BugReportDesiredAsApplied
          THEN /\ appliedPlan' = desiredPlan
               /\ badAppliedBeforeSuccess' = TRUE
          ELSE /\ appliedPlan' = appliedPlan
               /\ badAppliedBeforeSuccess' = badAppliedBeforeSuccess
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, connectRequested,
        applyFailureDone, syncedOnce, changeDone, relayUp, relayBag,
        published, delivered, badApplyStuckAfterFailure
      >>

ApplySuccess ==
    /\ refreshInFlight
    /\ LET completedPlan == applyingPlan
       IN
       /\ appliedPlan' = completedPlan
       /\ applyingPlan' = {}
       /\ refreshInFlight' = FALSE
       /\ refreshDirty' = (desiredPlan # completedPlan)
       /\ syncedOnce' = TRUE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, connectRequested,
        applyFailureDone, changeDone, relayUp, relayBag, published,
        delivered, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

ApplyFailureOrTimeout ==
    /\ refreshInFlight
    /\ ~applyFailureDone
    /\ refreshDirty' = TRUE
    /\ IF BugApplyNeverClearsInFlight
          THEN /\ refreshInFlight' = TRUE
               /\ applyingPlan' = applyingPlan
          ELSE /\ refreshInFlight' = FALSE
               /\ applyingPlan' = {}
    /\ applyFailureDone' = TRUE
    /\ badApplyStuckAfterFailure' =
        (badApplyStuckAfterFailure \/ BugApplyNeverClearsInFlight)
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, appliedPlan,
        connectRequested, syncedOnce, changeDone, relayUp, relayBag,
        published, delivered, badAppliedBeforeSuccess
      >>

PublishInbound(a) ==
    /\ a \in managerAuthors
    /\ a \notin published
    /\ relayBag' = relayBag \cup {a}
    /\ published' = published \cup {a}
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, connectRequested, applyFailureDone,
        syncedOnce, changeDone, relayUp, delivered, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

RelayDeliverByAppliedSubscription(a) ==
    /\ relayUp
    /\ a \in relayBag
    /\ a \in appliedPlan
    /\ relayBag' = relayBag \ {a}
    /\ delivered' = delivered \cup {a}
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, connectRequested, applyFailureDone,
        syncedOnce, changeDone, published, relayUp, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

CatchupDeliverByPlan(a) ==
    /\ relayUp
    /\ a \in relayBag
    /\ a \in ApplyTarget
    /\ relayBag' = relayBag \ {a}
    /\ delivered' = delivered \cup {a}
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, connectRequested, applyFailureDone,
        syncedOnce, changeDone, published, relayUp, badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, desiredPlan, applyingPlan, appliedPlan,
        refreshDirty, refreshInFlight, connectRequested, applyFailureDone,
        syncedOnce, changeDone, relayBag, published, delivered,
        badAppliedBeforeSuccess, badApplyStuckAfterFailure
      >>

RelayDelay ==
    /\ relayBag # {}
    /\ UNCHANGED vars

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ SessionStateChanges
    \/ RequestConnection
    \/ ConnectRelay
    \/ StartApply
    \/ ApplySuccess
    \/ ApplyFailureOrTimeout
    \/ \E a \in Authors: PublishInbound(a)
    \/ \E a \in Authors: RelayDeliverByAppliedSubscription(a)
    \/ \E a \in Authors: CatchupDeliverByPlan(a)
    \/ RelayPartition
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(SessionStateChanges)
    /\ WF_vars(RequestConnection)
    /\ WF_vars(ConnectRelay)
    /\ WF_vars(StartApply)
    /\ WF_vars(ApplySuccess)
    /\ \A a \in Authors: WF_vars(PublishInbound(a))
    /\ \A a \in Authors: SF_vars(RelayDeliverByAppliedSubscription(a))
    /\ \A a \in Authors: SF_vars(CatchupDeliverByPlan(a))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

SessionManagerDoesNotOwnDirectMessageSubscriptions ==
    managerDirectSubscriptionAuthors = {}

DesiredPlanMirrorsProtocolState ==
    desiredPlan = managerAuthors

AppliedPlanOnlyAfterSuccessfulApply ==
    ~badAppliedBeforeSuccess

ApplyFailureClearsInFlight ==
    ~badApplyStuckAfterFailure

CleanAppliedPlanMirrorsDesiredPlan ==
    (~refreshDirty /\ ~refreshInFlight) => appliedPlan = desiredPlan

NewAuthorsEnterDesiredPlanImmediately ==
    managerAuthors \subseteq desiredPlan

SkippedMessageAuthorsAreTracked ==
    managerSkippedAuthors \subseteq managerAuthors

ProtocolPlansContainKnownAuthors ==
    /\ desiredPlan \subseteq Authors
    /\ applyingPlan \subseteq Authors
    /\ appliedPlan \subseteq Authors

RefreshEventuallyCleanUnderRecovery ==
    [](refreshDirty => <> (~refreshDirty /\ ~refreshInFlight /\ appliedPlan = desiredPlan))

TrackedInboundEventuallyDeliveredUnderRecovery ==
    \A a \in Authors:
        [](a \in published /\ a \in managerAuthors => <> (a \in delivered \/ a \notin managerAuthors))

RemovedAuthorsEventuallyUnsubscribed ==
    \A a \in InitialMessageAuthors \ UpdatedMessageAuthors:
        [](changeDone => <> (a \notin desiredPlan /\ a \notin appliedPlan))

====
