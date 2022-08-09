/// Peer wire protocol automata

// a: () -> ACommit OKACommit.
// a: () -> AReveal.
// a: () -> OKRefundSigs RefundSigs.
// a: () -> OKcancelA.

// commit_a: ACommit OKACommit -> ACommitB ACommitB.
// reveal_a: ACommitB BCommitA AReveal  -> ARevealB ARevealB.
// rfndsigs_a: ARevealB CoreArbitA OKRefundSigs RefundSigs -> RfndProcSigB RfndProcSigB.
// cancel_a: RfndProcSigB OKcancelA -> ().

// b: () -> BCommit OKBCommit.
// b: () -> BReveal.
// b: () -> CoreArbit OKCoreArb.
// b: () -> OKbuy.
// b: () -> OKcancelB.

// commit_b: BCommit OKBCommit -> BCommitA BCommitA.
// reveal_b: ACommitB BCommitA BReveal -> BRevealA.
// corearb_b: BRevealA ARevealB OKCoreArb CoreArbit -> CoreArbitA CoreArbitA.
// buysig_b: CoreArbitA RfndProcSigB OKbuy  -> BuyA BuyAdaptorSigA.
// cancel_b: RfndProcSigB OKcancelB -> ().

/// Alice Interdaemon

trait CommitA<ACommit, OKACommit, ACommitB> {
    fn commit_a(ac: ACommit, ok_ac: OKACommit) -> ACommitB;
}

trait RevealA<ACommitB, BCommitA, AReveal, ARevealB> {
    fn reveal(acb: ACommitB, bca: BCommitA, ar: AReveal) -> ARevealB;
}

trait RfndProcSigs<ARevealB, CoreArbitA, OKRefundSigs, RefundSigs, RfndProcSigB> {
    fn rfndsigs(
        arb: ARevealB,
        corearbita: CoreArbitA,
        ok_rfndsigs: OKRefundSigs,
        rfndsigs: RefundSigs,
    ) -> RfndProcSigB;
}
trait CancelA<RfndProcSigB, OKcancelA> {
    fn cancel(rfndprocsigb: RfndProcSigB, ok_cancela: OKcancelA) -> ();
}

trait AliceProtocol<
    ACommit,
    OKACommit,
    ACommitB,
    BCommitA,
    AReveal,
    ARevealB,
    CoreArbitA,
    OKRefundSigs,
    RefundSigs,
    RfndProcSigB,
    OKcancelA,
>:
    CommitA<ACommit, OKACommit, ACommitB>
    + RevealA<ACommitB, BCommitA, AReveal, ARevealB>
    + RfndProcSigs<ARevealB, CoreArbitA, OKRefundSigs, RefundSigs, RfndProcSigB>
    + CancelA<RfndProcSigB, OKcancelA>
{
}

impl
    AliceProtocol<
        ACommit,
        OKACommit,
        ACommitB,
        BCommitA,
        AReveal,
        ARevealB,
        CoreArbitA,
        OKRefundSigs,
        RefundSigs,
        RfndProcSigB,
        OKcancelA,
    > for AInterdaemon
{
}

impl CommitA<ACommit, OKACommit, ACommitB> for AInterdaemon {
    fn commit_a(c: ACommit, pred: OKACommit) -> ACommitB {
        Default::default()
    }
}
impl RevealA<ACommitB, BCommitA, AReveal, ARevealB> for AInterdaemon {
    fn reveal(ac: ACommitB, bc: BCommitA, ar: AReveal) -> ARevealB {
        Default::default()
    }
}
impl RfndProcSigs<ARevealB, CoreArbitA, OKRefundSigs, RefundSigs, RfndProcSigB> for AInterdaemon {
    fn rfndsigs(
        arb: ARevealB,
        corearbita: CoreArbitA,
        okrfndsigs: OKRefundSigs,
        rfndsigs: RefundSigs,
    ) -> RfndProcSigB {
        Default::default()
    }
}
impl CancelA<RfndProcSigB, OKcancelA> for AInterdaemon {
    fn cancel(rfndsigs: RfndProcSigB, pred: OKcancelA) -> () {
        Default::default()
    }
}

#[derive(Default, Debug, PartialEq)]
struct ACommit;
#[derive(Default, Debug, PartialEq)]
struct OKACommit;
#[derive(Default, Debug, PartialEq)]
struct AReveal;
#[derive(Default, Debug, PartialEq)]
struct OKRefundSigs;
#[derive(Default, Debug, PartialEq)]
struct RefundSigs;
#[derive(Default, Debug, PartialEq)]
struct OKcancelA;
#[derive(Debug, PartialEq)]
enum AInterdaemon {
    StartA(),
    CommitA(ACommit, OKACommit),
    RevealA(ACommitB, BCommitA, AReveal),
    RfndProcSigsA(ARevealB, CoreArbitA, OKRefundSigs, RefundSigs),
    FinishA(),
}
impl AInterdaemon {
    fn run(self) -> Self {
        match self {
            Self::StartA() => {
                let (ac, ok_ac) = Default::default();
                dbg!(Self::CommitA(ac, ok_ac).run())
            }
            Self::CommitA(ac, ok_ac) => {
                let acb = Self::commit_a(ac, ok_ac);
                let (bca, ar) = Default::default();
                dbg!(Self::RevealA(acb, bca, ar).run())
            }
            Self::RevealA(acb, bca, ar) => {
                let arb = Self::reveal(acb, bca, ar);
                let (corearbita, ok_rfndsigs, rfndsigs) = Default::default();
                dbg!(Self::RfndProcSigsA(arb, corearbita, ok_rfndsigs, rfndsigs).run())
            }
            // base cases
            Self::RfndProcSigsA(arb, corearbita, ok_rfndsigs, rfndsigs) => {
                let _rfndprocsigb = Self::rfndsigs(arb, corearbita, ok_rfndsigs, rfndsigs);
                dbg!(Self::FinishA())
            }
            Self::FinishA() => self,
        }
    }
}

/// Bob interdaemon

trait CommitB<BCommit, OKBCommit, BCommitA> {
    fn commit_b(bc: BCommit, ok_bc: OKBCommit) -> BCommitA;
}
trait RevealB<ACommitB, BCommitA, BReveal, BRevealA> {
    fn reveal_b(acb: ACommitB, bca: BCommitA, br: BReveal) -> BRevealA;
}
trait CoreArbitr<BRevealA, ARevealB, OKCoreArb, CoreArbit, CoreArbitA> {
    fn core_arbitr(
        bra: BRevealA,
        arb: ARevealB,
        ok_corearb: OKCoreArb,
        corearb: CoreArbit,
    ) -> CoreArbitA;
}
trait BuySig<CoreArbitA, RfndProcSigB, OKbuy, BuyA, BuyAdaptorSigA> {
    fn buysig(
        corearbita: CoreArbitA,
        rfndprocsig: RfndProcSigB,
        ok_buy: OKbuy,
    ) -> (BuyA, BuyAdaptorSigA);
}
trait CancelB<RfndProcSigB, OKcancelB> {
    fn cancel_b(rfndprocsig: RfndProcSigB, pred: OKcancelB) -> ();
}
trait BobProtocol<
    BCommit,
    OKBCommit,
    BCommitA,
    ACommitB,
    BReveal,
    BRevealA,
    ARevealB,
    OKCoreArb,
    CoreArbit,
    CoreArbitA,
    RfndProcSigB,
    OKbuy,
    BuyA,
    BuyAdaptorSigA,
    OKcancelB,
>:
    CommitB<BCommit, OKBCommit, BCommitA>
    + RevealB<ACommitB, BCommitA, BReveal, BRevealA>
    + CoreArbitr<BRevealA, ARevealB, OKCoreArb, CoreArbit, CoreArbitA>
    + BuySig<CoreArbitA, RfndProcSigB, OKbuy, BuyA, BuyAdaptorSigA>
    + CancelB<RfndProcSigB, OKcancelB>
{
}

impl
    BobProtocol<
        BCommit,
        OKBCommit,
        BCommitA,
        ACommitB,
        BReveal,
        BRevealA,
        ARevealB,
        OKCoreArb,
        CoreArbit,
        CoreArbitA,
        RfndProcSigB,
        OKbuy,
        BuyA,
        BuyAdaptorSigA,
        OKcancelB,
    > for BInterdaemon
{
}

impl CommitB<BCommit, OKBCommit, BCommitA> for BInterdaemon {
    fn commit_b(bc: BCommit, ok_bc: OKBCommit) -> BCommitA {
        Default::default()
    }
}

impl RevealB<ACommitB, BCommitA, BReveal, BRevealA> for BInterdaemon {
    fn reveal_b(acb: ACommitB, bcb: BCommitA, br: BReveal) -> BRevealA {
        Default::default()
    }
}

impl CoreArbitr<BRevealA, ARevealB, OKCoreArb, CoreArbit, CoreArbitA> for BInterdaemon {
    fn core_arbitr(
        bra: BRevealA,
        arb: ARevealB,
        ok_corearb: OKCoreArb,
        corearb: CoreArbit,
    ) -> CoreArbitA {
        Default::default()
    }
}
impl BuySig<CoreArbitA, RfndProcSigB, OKbuy, BuyA, BuyAdaptorSigA> for BInterdaemon {
    fn buysig(
        corearb: CoreArbitA,
        rfndsigb: RfndProcSigB,
        ok_buy: OKbuy,
    ) -> (BuyA, BuyAdaptorSigA) {
        Default::default()
    }
}
impl CancelB<RfndProcSigB, OKcancelB> for BInterdaemon {
    fn cancel_b(rfndsigb: RfndProcSigB, okcancelb: OKcancelB) -> () {
        Default::default()
    }
}

#[derive(Default, Debug, PartialEq)]
struct BCommit;
#[derive(Default, Debug, PartialEq)]
struct OKBCommit;
#[derive(Default, Debug, PartialEq)]
struct BCommitA;
#[derive(Default, Debug, PartialEq)]
struct ACommitB;
#[derive(Default, Debug, PartialEq)]
struct BReveal;
#[derive(Default, Debug, PartialEq)]
struct BRevealA;
#[derive(Default, Debug, PartialEq)]
struct ARevealB;
#[derive(Default, Debug, PartialEq)]
struct OKCoreArb;
#[derive(Default, Debug, PartialEq)]
struct CoreArbit;
#[derive(Default, Debug, PartialEq)]
struct CoreArbitA;
#[derive(Default, Debug, PartialEq)]
struct RfndProcSigB;
#[derive(Default, Debug, PartialEq)]
struct OKbuy;
#[derive(Default, Debug, PartialEq)]
struct BuyA;
#[derive(Default, Debug, PartialEq)]
struct BuyAdaptorSigA;
#[derive(Default, Debug, PartialEq)]
struct OKcancelB;

#[derive(Debug, PartialEq)]
enum BInterdaemon {
    StartB(),
    CommitB(BCommit, OKBCommit),
    RevealB(ACommitB, BCommitA, BReveal),
    CorearbB(BRevealA, ARevealB, OKCoreArb, CoreArbit),
    BuyProcSigB(CoreArbitA, RfndProcSigB, OKbuy),
    FinishB(),
}

impl BInterdaemon {
    fn run(self) -> Self {
        match self {
            Self::StartB() => {
                let (bc, ok_bc) = Default::default();
                dbg!(Self::CommitB(bc, ok_bc).run())
            }
            Self::CommitB(bc, ok_bc) => {
                let bca = Self::commit_b(bc, ok_bc);
                let (acb, br) = Default::default();
                dbg!(Self::RevealB(acb, bca, br).run())
            }
            Self::RevealB(acb, bca, br) => {
                let bra = Self::reveal_b(acb, bca, br);
                let (arb, ok_corearb, corearb) = Default::default();
                dbg!(Self::CorearbB(bra, arb, ok_corearb, corearb).run())
            }
            Self::CorearbB(bra, arb, ok_corearb, corearb) => {
                let corearbita = Self::core_arbitr(bra, arb, ok_corearb, corearb);
                let (rfndprocsigb, ok_buy) = Default::default();
                dbg!(Self::BuyProcSigB(corearbita, rfndprocsigb, ok_buy).run())
            }
            // base cases
            Self::BuyProcSigB(corearbita, rfndprocsigb, ok_buy) => {
                let _ = Self::buysig(corearbita, rfndprocsigb, ok_buy);
                dbg!(Self::FinishB())
            }
            Self::FinishB() => dbg!(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AInterdaemon, BInterdaemon};
    #[test]
    fn run_net_a() {
        assert_eq!(
            AInterdaemon::run(AInterdaemon::StartA()),
            AInterdaemon::FinishA()
        );
    }
    #[test]
    fn run_net_b() {
        assert_eq!(
            BInterdaemon::run(BInterdaemon::StartB()),
            BInterdaemon::FinishB()
        );
    }
}





