using GitAiVS.Listeners;
using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace GitAiVS.Tests
{
    [TestClass]
    public class DocumentSaveListenerTests
    {
        [TestMethod]
        public void IsInternalPathReturnsTrueForVsDirectory()
        {
            Assert.IsTrue(DocumentSaveListener.IsInternalPath(@"C:\project\.vs\settings.json"));
        }

        [TestMethod]
        public void IsInternalPathReturnsTrueForUnixVsDirectory()
        {
            Assert.IsTrue(DocumentSaveListener.IsInternalPath("/project/.vs/settings.json"));
        }

        [TestMethod]
        public void IsInternalPathReturnsFalseForNormalFile()
        {
            Assert.IsFalse(DocumentSaveListener.IsInternalPath(@"C:\project\src\Program.cs"));
        }

        [TestMethod]
        public void IsInternalPathReturnsFalseForVsCodeDirectory()
        {
            Assert.IsFalse(DocumentSaveListener.IsInternalPath(@"C:\project\.vscode\settings.json"));
        }

        [TestMethod]
        public void IsInternalPathReturnsFalseForPartialMatch()
        {
            Assert.IsFalse(DocumentSaveListener.IsInternalPath(@"C:\project\devs\file.txt"));
        }
    }
}
